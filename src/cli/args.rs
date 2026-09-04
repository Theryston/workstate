use std::{ffi::OsString, path::PathBuf, str::FromStr};

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Clone, Parser)]
#[command(
    name = "workstate",
    version,
    about = "Launch your complete development environment with one command"
)]
pub struct Cli {
    #[command(flatten)]
    pub options: GlobalOptions,
    #[command(subcommand)]
    pub subcommand: Option<CliSubcommand>,
    #[arg(value_name = "ENVIRONMENT", index = 1)]
    pub environment: Option<EnvironmentArgument>,
}

#[derive(Debug, Clone, Args, Default, PartialEq, Eq)]
pub struct GlobalOptions {
    #[arg(long, global = true, help = "Skip destructive confirmations")]
    pub yes: bool,
    #[arg(long, global = true, help = "Show the plan without applying changes")]
    pub dry_run: bool,
    #[arg(long, global = true, help = "Emit machine-readable JSON")]
    pub json: bool,
    #[arg(long, global = true, help = "Suppress non-error output")]
    pub quiet: bool,
    #[arg(long, global = true, help = "Enable verbose diagnostics")]
    pub verbose: bool,
    #[arg(long, global = true, help = "Disable terminal colors")]
    pub no_color: bool,
    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = "Use a custom Workstate data directory"
    )]
    pub config: Option<PathBuf>,
}

#[derive(Debug, Clone, Subcommand, PartialEq, Eq)]
pub enum CliSubcommand {
    #[command(about = "Create or edit an environment")]
    Add {
        #[arg(value_name = "ENVIRONMENT")]
        environment: EnvironmentArgument,
    },
    #[command(about = "Stop an environment")]
    Stop {
        #[arg(value_name = "ENVIRONMENT")]
        environment: EnvironmentArgument,
    },
    #[command(about = "Stop and delete an environment")]
    Delete {
        #[arg(value_name = "ENVIRONMENT")]
        environment: EnvironmentArgument,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EnvironmentArgument(String);

impl EnvironmentArgument {
    pub fn new(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        if value.is_empty() || value.chars().all(char::is_whitespace) {
            return Err("environment must not be empty".to_owned());
        }
        if value == "." || value == ".." {
            return Err("environment cannot be . or ..".to_owned());
        }
        if value.chars().any(|character| {
            character == '/' || character == '\\' || character == '\0' || character.is_control()
        }) {
            return Err(
                "environment cannot contain path separators or control characters".to_owned(),
            );
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for EnvironmentArgument {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl From<EnvironmentArgument> for OsString {
    fn from(value: EnvironmentArgument) -> Self {
        OsString::from(value.0)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use clap::Parser;

    use super::{Cli, CliSubcommand, EnvironmentArgument, GlobalOptions};

    #[test]
    fn parses_global_options_with_defaults() {
        let parsed = Cli::try_parse_from(["workstate"]);
        assert!(parsed.is_ok());
        let Some(parsed) = parsed.ok() else {
            return;
        };
        assert_eq!(parsed.options, GlobalOptions::default());
        assert!(parsed.subcommand.is_none());
        assert!(parsed.environment.is_none());
    }

    #[test]
    fn reserves_known_subcommands_before_the_positional_environment() {
        let parsed = Cli::try_parse_from(["workstate", "add", "personal-blog"]);
        assert!(parsed.is_ok());
        let Some(parsed) = parsed.ok() else {
            return;
        };
        let expected_environment = EnvironmentArgument::new("personal-blog");
        assert!(expected_environment.is_ok());
        let Some(expected_environment) = expected_environment.ok() else {
            return;
        };
        assert_eq!(
            parsed.subcommand,
            Some(CliSubcommand::Add {
                environment: expected_environment,
            })
        );
        assert!(parsed.environment.is_none());
    }

    #[test]
    fn parses_each_public_subcommand_and_positional_start() {
        assert!(Cli::try_parse_from(["workstate", "my-app"]).is_ok());
        assert!(Cli::try_parse_from(["workstate", "stop", "my-app"]).is_ok());
        assert!(Cli::try_parse_from(["workstate", "delete", "my-app"]).is_ok());
        assert!(Cli::try_parse_from(["workstate", "--yes", "add", "my-app"]).is_ok());
    }

    #[test]
    fn parses_all_global_flags_without_changing_the_command_shape() {
        let parsed = Cli::try_parse_from([
            "workstate",
            "--yes",
            "--dry-run",
            "--json",
            "--quiet",
            "--verbose",
            "--no-color",
            "--config",
            "/tmp/workstate-data",
            "my-app",
        ]);
        assert!(parsed.is_ok());
        let Some(parsed) = parsed.ok() else {
            return;
        };
        assert!(parsed.options.yes);
        assert!(parsed.options.dry_run);
        assert!(parsed.options.json);
        assert!(parsed.options.quiet);
        assert!(parsed.options.verbose);
        assert!(parsed.options.no_color);
        assert_eq!(
            parsed.options.config,
            Some(PathBuf::from("/tmp/workstate-data"))
        );
        assert!(parsed.environment.is_some());
    }

    #[test]
    fn rejects_invalid_environment_arguments() {
        assert!(Cli::try_parse_from(["workstate", "../outside"]).is_err());
        assert!(Cli::try_parse_from(["workstate", "add", "../outside"]).is_err());
    }
}
