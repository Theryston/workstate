use std::path::PathBuf;

use clap::Parser;
use workstate::cli::{
    args::{Cli, CliSubcommand},
    command::{Command, parse_from},
    output::{BufferOutput, OutputFormat, OutputPolicy},
};
use workstate::error::{ErrorCategory, WorkstateError};

#[test]
fn public_command_grammar_covers_selection_start_add_stop_and_delete() {
    let cases = [
        (vec!["workstate"], "select"),
        (vec!["workstate", "personal-blog"], "start"),
        (vec!["workstate", "add", "personal-blog"], "add"),
        (vec!["workstate", "stop", "personal-blog"], "stop"),
        (vec!["workstate", "delete", "personal-blog"], "delete"),
    ];

    for (arguments, expected) in cases {
        let invocation = parse_from(arguments);
        assert!(invocation.is_ok());
        let Some(invocation) = invocation.ok() else {
            continue;
        };
        let actual = match invocation.command {
            Command::Select => "select",
            Command::Start { .. } => "start",
            Command::Add { .. } => "add",
            Command::Stop { .. } => "stop",
            Command::Delete { .. } => "delete",
        };
        assert_eq!(actual, expected);
    }
}

#[test]
fn subcommand_names_are_reserved_and_environment_names_are_validated() {
    let parsed = Cli::try_parse_from(["workstate", "add", "personal-blog"]);
    assert!(parsed.is_ok());
    let Some(parsed) = parsed.ok() else {
        return;
    };
    assert!(matches!(parsed.subcommand, Some(CliSubcommand::Add { .. })));
    assert!(Cli::try_parse_from(["workstate", "../outside"]).is_err());
    assert!(Cli::try_parse_from(["workstate", "add", "../outside"]).is_err());
}

#[test]
fn all_global_flags_remain_available_at_the_public_parser_boundary() {
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
        "personal-blog",
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
}

#[test]
fn output_policy_keeps_machine_output_separate_from_human_output() {
    let policy = OutputPolicy {
        format: OutputFormat::Json,
        quiet: false,
        color: false,
    };
    let mut output = BufferOutput::default();
    assert!(
        policy
            .write_message(&mut output, "Environment ready")
            .is_ok()
    );
    assert!(output.stdout.contains("\"message\": \"Environment ready\""));
    let error = WorkstateError::new(ErrorCategory::Cli, "invalid command");
    assert!(policy.write_error(&mut output, &error).is_ok());
    assert!(output.stderr.contains("\"category\": \"CLI error\""));
}
