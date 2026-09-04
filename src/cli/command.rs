use std::ffi::OsString;

use clap::{Parser, error::ErrorKind};

use crate::error::{ErrorCategory, Result, WorkstateError};

use super::args::{Cli, CliSubcommand, EnvironmentArgument, GlobalOptions};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub command: Command,
    pub options: GlobalOptions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Select,
    Start {
        environment: EnvironmentArgument,
    },
    New {
        environment: Option<EnvironmentArgument>,
    },
    Edit {
        environment: Option<EnvironmentArgument>,
    },
    Stop {
        environment: Option<EnvironmentArgument>,
    },
    Delete {
        environment: Option<EnvironmentArgument>,
    },
}

pub fn parse_from<I, T>(arguments: I) -> Result<Invocation>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let parsed = Cli::try_parse_from(arguments).map_err(|source| {
        WorkstateError::with_source(ErrorCategory::Cli, source.to_string(), source)
    })?;
    Invocation::try_from(parsed)
}

pub fn meta_output(arguments: Vec<OsString>) -> Option<String> {
    match Cli::try_parse_from(arguments) {
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            Some(error.to_string())
        }
        Ok(_) | Err(_) => None,
    }
}

impl TryFrom<Cli> for Invocation {
    type Error = WorkstateError;

    fn try_from(cli: Cli) -> Result<Self> {
        let command = match (cli.subcommand, cli.environment) {
            (None, None) => Command::Select,
            (None, Some(environment)) => Command::Start { environment },
            (Some(CliSubcommand::Run { environment }), None)
            | (Some(CliSubcommand::Start { environment }), None) => match environment {
                Some(environment) => Command::Start { environment },
                None => Command::Select,
            },
            (Some(CliSubcommand::New { environment }), None) => Command::New { environment },
            (Some(CliSubcommand::Edit { environment }), None) => Command::Edit { environment },
            (Some(CliSubcommand::Stop { environment }), None) => Command::Stop { environment },
            (Some(CliSubcommand::Delete { environment }), None) => Command::Delete { environment },
            (Some(_), Some(_)) => {
                return Err(WorkstateError::new(
                    ErrorCategory::Cli,
                    "an environment argument cannot be combined with a subcommand",
                ));
            }
        };

        Ok(Self {
            command,
            options: cli.options,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{Command, meta_output, parse_from};

    #[test]
    fn converts_the_positional_environment_into_start() {
        let invocation = parse_from(["workstate", "personal-blog"]);
        assert!(invocation.is_ok());
        let Some(invocation) = invocation.ok() else {
            return;
        };
        assert!(matches!(invocation.command, Command::Start { .. }));
    }

    #[test]
    fn converts_an_empty_invocation_into_selection() {
        let invocation = parse_from(["workstate"]);
        assert!(invocation.is_ok());
        let Some(invocation) = invocation.ok() else {
            return;
        };
        assert_eq!(invocation.command, Command::Select);
    }

    #[test]
    fn hidden_run_aliases_normalize_to_the_base_command() {
        for arguments in [["workstate", "run"], ["workstate", "start"]] {
            let invocation = parse_from(arguments);
            assert!(invocation.is_ok());
            let Some(invocation) = invocation.ok() else {
                continue;
            };
            assert_eq!(invocation.command, Command::Select);
        }

        for arguments in [
            ["workstate", "run", "personal-blog"],
            ["workstate", "start", "personal-blog"],
        ] {
            let invocation = parse_from(arguments);
            assert!(invocation.is_ok());
            let Some(invocation) = invocation.ok() else {
                continue;
            };
            assert!(matches!(invocation.command, Command::Start { .. }));
        }
    }

    #[test]
    fn keeps_optional_environment_targets_for_subcommands() {
        for (arguments, expected) in [
            (["workstate", "new"], "new"),
            (["workstate", "edit"], "edit"),
            (["workstate", "stop"], "stop"),
            (["workstate", "delete"], "delete"),
        ] {
            let invocation = parse_from(arguments);
            assert!(invocation.is_ok());
            let Some(invocation) = invocation.ok() else {
                continue;
            };
            let command = match invocation.command {
                Command::New { environment: None } => "new",
                Command::Edit { environment: None } => "edit",
                Command::Stop { environment: None } => "stop",
                Command::Delete { environment: None } => "delete",
                _ => "unexpected",
            };
            assert_eq!(command, expected);
        }
    }

    #[test]
    fn preserves_global_flags_in_the_typed_invocation() {
        let invocation = parse_from([
            "workstate",
            "--yes",
            "--dry-run",
            "--json",
            "delete",
            "personal-blog",
        ]);
        assert!(invocation.is_ok());
        let Some(invocation) = invocation.ok() else {
            return;
        };
        assert!(matches!(invocation.command, Command::Delete { .. }));
        assert!(invocation.options.yes);
        assert!(invocation.options.dry_run);
        assert!(invocation.options.json);
    }

    #[test]
    fn recognizes_help_and_version_without_treating_them_as_lifecycle_commands() {
        assert!(meta_output(vec![OsString::from("workstate"), OsString::from("--help")]).is_some());
        assert!(
            meta_output(vec![
                OsString::from("workstate"),
                OsString::from("--version")
            ])
            .is_some()
        );
    }
}
