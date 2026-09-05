use std::path::PathBuf;

use crate::{
    application::ports::ProcessRequest,
    domain::{ActionId, CommandSpec},
    error::{ErrorCategory, Result, WorkstateError},
};

pub fn to_process_request(
    specification: &CommandSpec,
    working_directory: Option<PathBuf>,
) -> Result<ProcessRequest> {
    let specification = normalize_command_line(specification)?;
    validate_specification(&specification)?;
    if specification.shell {
        if !specification.arguments.is_empty() {
            return Err(WorkstateError::new(
                ErrorCategory::Process,
                "shell commands must not define argv arguments",
            ));
        }
        return Ok(ProcessRequest {
            program: shell_program(),
            arguments: vec!["-c".to_owned(), specification.program.clone()],
            working_directory,
            environment: environment_entries(&specification),
        });
    }

    Ok(ProcessRequest {
        program: specification.program.clone(),
        arguments: specification.arguments.clone(),
        working_directory,
        environment: environment_entries(&specification),
    })
}

fn normalize_command_line(specification: &CommandSpec) -> Result<CommandSpec> {
    if specification.shell
        || !specification.arguments.is_empty()
        || !specification.program.chars().any(char::is_whitespace)
    {
        return Ok(specification.clone());
    }

    let action_id = ActionId::new("command").map_err(WorkstateError::from)?;
    CommandSpec::from_argv_line(&action_id, &specification.program)
        .map_err(WorkstateError::from)
        .map(|mut command| {
            command.environment = specification.environment.clone();
            command.shell = specification.shell;
            command
        })
}

fn validate_specification(specification: &CommandSpec) -> Result<()> {
    if specification.program.is_empty() || specification.program.chars().any(char::is_control) {
        return Err(WorkstateError::new(
            ErrorCategory::Process,
            "the executable or shell command must be non-empty and contain no control characters",
        ));
    }
    if specification
        .arguments
        .iter()
        .any(|argument| argument.contains('\0') || argument.chars().any(char::is_control))
    {
        return Err(WorkstateError::new(
            ErrorCategory::Process,
            "command arguments must not contain control characters",
        ));
    }
    if specification.environment.iter().any(|(key, value)| {
        key.is_empty()
            || key.contains('=')
            || key.chars().any(char::is_control)
            || value.chars().any(char::is_control)
    }) {
        return Err(WorkstateError::new(
            ErrorCategory::Process,
            "environment entries must have valid names and contain no control characters",
        ));
    }
    Ok(())
}

pub fn shell_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for character in value.chars() {
        if character == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(character);
        }
    }
    quoted.push('\'');
    quoted
}

fn environment_entries(specification: &CommandSpec) -> Vec<(String, String)> {
    specification
        .environment
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn shell_program() -> String {
    std::env::var_os("SHELL")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/bin/sh".to_owned())
}

#[cfg(test)]
mod tests {
    use super::to_process_request;
    use crate::domain::CommandSpec;

    #[test]
    fn argv_mode_preserves_program_and_argument_boundaries() {
        let mut command = CommandSpec::new("bun");
        command.arguments = vec!["run".to_owned(), "dev server".to_owned()];
        let request = to_process_request(&command, None);
        assert!(request.is_ok());
        let Some(request) = request.ok() else {
            return;
        };
        assert_eq!(request.program, "bun");
        assert_eq!(request.arguments, vec!["run", "dev server"]);
    }

    #[test]
    fn shell_mode_keeps_the_exact_configured_command() {
        let mut command = CommandSpec::new("printf 'hello world'");
        command.shell = true;
        let request = to_process_request(&command, None);
        assert!(request.is_ok());
        let Some(request) = request.ok() else {
            return;
        };
        assert_eq!(request.arguments.first().map(String::as_str), Some("-c"));
        assert_eq!(
            request.arguments.get(1).map(String::as_str),
            Some(command.program.as_str())
        );
    }

    #[test]
    fn shell_mode_rejects_argv_arguments() {
        let mut command = CommandSpec::new("printf ok");
        command.shell = true;
        command.arguments.push("unexpected".to_owned());
        assert!(to_process_request(&command, None).is_err());
    }

    #[test]
    fn a_command_line_saved_as_the_program_is_recovered_as_argv() {
        let command = CommandSpec::new("bun i");
        let request = to_process_request(&command, None);
        assert!(request.is_ok());
        let Some(request) = request.ok() else {
            return;
        };
        assert_eq!(request.program, "bun");
        assert_eq!(request.arguments, vec!["i"]);
    }
}
