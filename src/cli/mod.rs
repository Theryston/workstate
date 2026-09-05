pub mod args;
pub mod command;
pub mod output;

use std::{ffi::OsString, future::Future, sync::Arc};

use crate::{
    application::context::AppContext,
    application::{
        reconciliation::{
            ChannelEventSink, EventSink, InMemoryEventSink, LifecycleRunResult, StopResult,
        },
        use_cases,
    },
    domain::{EnvironmentConfig, EnvironmentName, EnvironmentSlug, ExecutionMode},
    error::{ErrorCategory, Result, WorkstateError},
    infrastructure::persistence::WorkstatePaths,
    ui::{
        ApplicationProgressEventSource, EditorMode, EditorOutcome, EditorState,
        EnvironmentListItem, EnvironmentStatus, ProgressOperation, SelectorState, confirm_delete,
        edit_environment as run_editor, prompt_text, select_environment, show_lifecycle_progress,
    },
};

use self::{
    args::EnvironmentArgument,
    command::{Command, Invocation, parse_from},
    output::{ConsoleOutput, OutputFormat, OutputPolicy, OutputSink},
};

pub(crate) async fn run(context: AppContext) -> Result<()> {
    run_with_args(context, std::env::args_os().collect()).await
}

pub(crate) async fn run_with_args(context: AppContext, arguments: Vec<OsString>) -> Result<()> {
    let invocation = parse_from(arguments)?;
    context.preflight()?;
    let context = match invocation.options.config.clone() {
        Some(root) => context.with_config_root(root)?,
        None => context,
    };
    dispatch(&context, invocation).await
}

pub fn render_error_for_args(arguments: &[OsString], error: &WorkstateError) -> String {
    let policy = parse_from(arguments.iter().cloned())
        .map(|invocation| OutputPolicy::from_options(&invocation.options))
        .unwrap_or_else(|_| OutputPolicy::from_options(&args::GlobalOptions::default()));
    policy
        .render_error(error)
        .unwrap_or_else(|_| error.render())
}

async fn dispatch(context: &AppContext, invocation: Invocation) -> Result<()> {
    let policy = OutputPolicy::from_options(&invocation.options);
    let mut output = ConsoleOutput;

    match invocation.command {
        Command::Select => {
            let selector = load_selector_state(context)?;
            let Some(environment) = select_environment(selector, invocation.options.no_color)?
            else {
                return Ok(());
            };
            run_environment(
                context,
                &environment,
                &invocation.options,
                &policy,
                &mut output,
            )
            .await
        }
        Command::Start { environment } => {
            let slug = resolve_environment_slug(environment.as_str())?;
            run_environment(context, &slug, &invocation.options, &policy, &mut output).await
        }
        Command::New { environment } => {
            let Some(argument) = select_or_prompt_new_environment(
                environment.as_ref(),
                invocation.options.no_color,
            )?
            else {
                return Ok(());
            };
            new_environment(
                context,
                argument.as_str(),
                &invocation.options,
                &policy,
                &mut output,
            )
            .await
        }
        Command::Edit { environment } => {
            let Some(environment) = select_existing_environment(
                context,
                environment.as_ref(),
                invocation.options.no_color,
            )?
            else {
                return Ok(());
            };
            edit_environment_command(
                context,
                environment.as_str(),
                &invocation.options,
                &policy,
                &mut output,
            )
            .await
        }
        Command::Stop { environment } => {
            let Some(environment) = select_existing_environment(
                context,
                environment.as_ref(),
                invocation.options.no_color,
            )?
            else {
                return Ok(());
            };
            stop_environment(
                context,
                environment.as_str(),
                &invocation.options,
                &policy,
                &mut output,
            )
            .await
        }
        Command::Delete { environment } => {
            let Some(environment) = select_existing_environment(
                context,
                environment.as_ref(),
                invocation.options.no_color,
            )?
            else {
                return Ok(());
            };
            delete_environment(
                context,
                environment.as_str(),
                &invocation.options,
                &policy,
                &mut output,
            )
            .await
        }
    }
}

fn select_existing_environment(
    context: &AppContext,
    argument: Option<&EnvironmentArgument>,
    no_color: bool,
) -> Result<Option<EnvironmentSlug>> {
    match argument {
        Some(argument) => resolve_environment_slug(argument.as_str()).map(Some),
        None => select_environment(load_selector_state(context)?, no_color),
    }
}

fn select_or_prompt_new_environment(
    argument: Option<&EnvironmentArgument>,
    no_color: bool,
) -> Result<Option<EnvironmentArgument>> {
    match argument {
        Some(argument) => Ok(Some(argument.clone())),
        None => prompt_text("Environment name", None, no_color, |value| {
            EnvironmentConfig::new(value.as_str())
                .map(|_| ())
                .map_err(|error| error.to_string())
        })?
        .map(EnvironmentArgument::new)
        .transpose()
        .map_err(|error| WorkstateError::new(ErrorCategory::Cli, error)),
    }
}

fn load_selector_state(context: &AppContext) -> Result<SelectorState> {
    let slugs = context.config_store().list()?;
    let mut items = Vec::with_capacity(slugs.len());
    for slug in slugs {
        let configuration = context.config_store().load(&slug)?;
        let name = configuration
            .as_ref()
            .map(|configuration| configuration.name.clone())
            .map(Ok)
            .unwrap_or_else(|| EnvironmentName::new(slug.as_str()).map_err(WorkstateError::from))?;
        let runtime = context.state_store().load(&slug)?;
        items.push(EnvironmentListItem::new(
            name,
            slug,
            EnvironmentStatus::from_runtime(runtime.as_ref()),
        ));
    }
    Ok(SelectorState::new(items))
}

async fn run_environment(
    context: &AppContext,
    slug: &EnvironmentSlug,
    options: &args::GlobalOptions,
    policy: &OutputPolicy,
    output: &mut dyn OutputSink,
) -> Result<()> {
    let Some(configuration) = context.config_store().load(slug)? else {
        return Err(environment_not_found(slug.as_str()));
    };
    let background = configuration
        .actions
        .iter()
        .any(|action| action.execution_mode == Some(ExecutionMode::Background));
    let result = if !options.json && !options.quiet {
        execute_with_progress(
            &configuration,
            ProgressOperation::Run,
            options.no_color,
            |events| use_cases::run::execute(context, slug, options.dry_run, events),
        )
        .await?
    } else {
        let events = Arc::new(InMemoryEventSink::default());
        use_cases::run::execute(context, slug, options.dry_run, events).await?
    };
    let message = match policy.format {
        OutputFormat::Human => render_run_summary(&configuration, slug, &result, background),
        OutputFormat::Json => {
            if options.dry_run {
                format!(
                    "Dry run complete for '{}': {} change(s) would be applied.",
                    configuration.name, result.report.planned_change_count
                )
            } else if result.report.planned_change_count == 0 {
                format!(
                    "Environment '{}' is already in the desired state.",
                    configuration.name
                )
            } else {
                let mut message = format!("Environment '{}' is ready.", configuration.name);
                if background {
                    message.push_str(&format!(
                        "\n\nInspect background processes with:\n  tmux attach-session -t workstate-{}\n\nStop with:\n  workstate stop {}",
                        slug, slug
                    ));
                }
                message
            }
        }
    };
    policy.write_message(output, &message)
}

async fn new_environment(
    context: &AppContext,
    argument: &str,
    options: &args::GlobalOptions,
    policy: &OutputPolicy,
    output: &mut dyn OutputSink,
) -> Result<()> {
    let slug = resolve_environment_slug(argument)?;
    if context.config_store().load(&slug)?.is_some() {
        return Err(environment_already_exists(argument));
    }

    if options.dry_run {
        return policy.write_message(
            output,
            &format!("Dry run: the editor would create '{slug}'."),
        );
    }

    let configuration = EnvironmentConfig::new(argument).map_err(WorkstateError::from)?;
    open_environment_editor(
        context,
        configuration,
        EditorMode::Create,
        options,
        policy,
        output,
    )
    .await
}

async fn edit_environment_command(
    context: &AppContext,
    argument: &str,
    options: &args::GlobalOptions,
    policy: &OutputPolicy,
    output: &mut dyn OutputSink,
) -> Result<()> {
    let slug = resolve_environment_slug(argument)?;
    let Some(configuration) = context.config_store().load(&slug)? else {
        return Err(environment_not_found(argument));
    };

    if options.dry_run {
        return policy.write_message(output, &format!("Dry run: the editor would edit '{slug}'."));
    }

    open_environment_editor(
        context,
        configuration,
        EditorMode::Edit,
        options,
        policy,
        output,
    )
    .await
}

async fn open_environment_editor(
    context: &AppContext,
    configuration: EnvironmentConfig,
    mode: EditorMode,
    options: &args::GlobalOptions,
    policy: &OutputPolicy,
    output: &mut dyn OutputSink,
) -> Result<()> {
    let editor = match context.desktop_backend().snapshot().await {
        Ok(snapshot) => {
            EditorState::new(configuration, mode).with_live_workspaces(snapshot.workspaces)
        }
        Err(error) => {
            EditorState::new(configuration, mode).with_workspace_observation_error(error.render())
        }
    };
    let editor = match context.application_catalog().list() {
        Ok(applications) => editor.with_installed_applications(applications),
        Err(error) => editor.with_application_observation_error(error.render()),
    };
    match run_editor(editor, options.no_color)? {
        EditorOutcome::Cancelled => Ok(()),
        EditorOutcome::Saved(configuration) => {
            match mode {
                EditorMode::Create => context.config_store().create(&configuration)?,
                EditorMode::Edit => context.config_store().save(&configuration)?,
            }
            policy.write_message(
                output,
                &format!("Environment '{}' saved.", configuration.name),
            )
        }
    }
}

async fn stop_environment(
    context: &AppContext,
    argument: &str,
    options: &args::GlobalOptions,
    policy: &OutputPolicy,
    output: &mut dyn OutputSink,
) -> Result<()> {
    let slug = resolve_environment_slug(argument)?;
    let Some(configuration) = context.config_store().load(&slug)? else {
        return Err(environment_not_found(argument));
    };
    let runtime = context.state_store().load(&slug)?;
    let active = runtime
        .as_ref()
        .is_some_and(|state| state.status.is_active());
    if options.dry_run {
        let message = match policy.format {
            OutputFormat::Human => render_stop_dry_run_summary(&configuration, &slug, active),
            OutputFormat::Json => format!(
                "Dry run: '{}' would be stopped. Active resources detected: {active}.",
                configuration.name
            ),
        };
        return policy.write_message(output, &message);
    }
    let result = if !options.json && !options.quiet {
        execute_with_progress(
            &configuration,
            ProgressOperation::Stop,
            options.no_color,
            |events| use_cases::stop::execute(context, &slug, events),
        )
        .await?
    } else {
        let events = Arc::new(InMemoryEventSink::default());
        use_cases::stop::execute(context, &slug, events).await?
    };
    let message = match policy.format {
        OutputFormat::Human => render_stop_summary(&configuration, &slug, &result),
        OutputFormat::Json => format!(
            "Environment '{}' stopped. Cleaned {} resource(s); preserved {}.",
            configuration.name, result.cleaned_resources, result.preserved_resources
        ),
    };
    policy.write_message(output, &message)
}

async fn delete_environment(
    context: &AppContext,
    argument: &str,
    options: &args::GlobalOptions,
    policy: &OutputPolicy,
    output: &mut dyn OutputSink,
) -> Result<()> {
    let slug = resolve_environment_slug(argument)?;
    let Some(configuration) = context.config_store().load(&slug)? else {
        return Err(environment_not_found(argument));
    };
    let runtime = context.state_store().load(&slug)?;
    let active = runtime
        .as_ref()
        .is_some_and(|state| state.status.is_active());
    let paths = WorkstatePaths::from_file_system(context.file_system())?;
    let environment_paths = paths.environment(&slug)?;

    if options.dry_run {
        return policy.write_message(
            output,
            &format!(
                "Dry run: '{}' would stop active resources and remove {}.",
                configuration.name,
                environment_paths.directory().display()
            ),
        );
    }

    if !options.yes
        && !confirm_delete(
            &configuration.name,
            environment_paths.directory(),
            active,
            options.no_color,
        )?
    {
        return Ok(());
    }

    let events = Arc::new(InMemoryEventSink::default());
    use_cases::delete::execute(context, &slug, events).await?;
    policy.write_message(
        output,
        &format!("Environment '{}' deleted.", configuration.name),
    )
}

fn resolve_environment_slug(argument: &str) -> Result<EnvironmentSlug> {
    if let Ok(slug) = EnvironmentSlug::new(argument) {
        return Ok(slug);
    }
    EnvironmentName::new(argument)
        .map_err(WorkstateError::from)?
        .derive_slug()
        .map_err(WorkstateError::from)
}

async fn execute_with_progress<T, F, Fut>(
    configuration: &EnvironmentConfig,
    operation: ProgressOperation,
    no_color: bool,
    execute: F,
) -> Result<T>
where
    F: FnOnce(Arc<dyn EventSink>) -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let (event_sink, receiver) = ChannelEventSink::bounded(512)?;
    let runtime = tokio::runtime::Handle::current();
    let progress_configuration = configuration.clone();
    let progress_task = tokio::task::spawn_blocking(move || {
        let mut source = ApplicationProgressEventSource::new(receiver, runtime);
        show_lifecycle_progress(&progress_configuration, &mut source, operation, no_color)
    });
    let lifecycle_result = execute(Arc::new(event_sink));
    let (lifecycle_result, progress_result) = tokio::join!(lifecycle_result, progress_task);

    match lifecycle_result {
        Err(error) => Err(error),
        Ok(value) => {
            let progress_result = progress_result.map_err(|source| {
                WorkstateError::with_source(
                    ErrorCategory::Ui,
                    "lifecycle progress task failed",
                    source,
                )
            })?;
            progress_result?;
            Ok(value)
        }
    }
}

fn environment_not_found(argument: &str) -> WorkstateError {
    WorkstateError::new(
        ErrorCategory::Persistence,
        format!("environment '{argument}' was not found"),
    )
    .with_context("suggested_command", format!("workstate new {argument}"))
}

fn environment_already_exists(argument: &str) -> WorkstateError {
    WorkstateError::new(
        ErrorCategory::Persistence,
        format!(
            "environment '{argument}' already exists. To edit it, use:\n  workstate edit {argument}"
        ),
    )
}

fn render_run_summary(
    configuration: &EnvironmentConfig,
    slug: &EnvironmentSlug,
    result: &LifecycleRunResult,
    background: bool,
) -> String {
    if result.report.dry_run {
        let rows = vec![
            format!(
                "◌ {} would be applied",
                count_text(result.report.planned_change_count, "change", "changes")
            ),
            format!(
                "• {} already correct",
                count_text(result.report.already_correct_count, "action", "actions")
            ),
            format!(
                "○ {} skipped",
                count_text(result.report.skipped_count, "action", "actions")
            ),
            format!("⏱ Completed in {} ms", result.report.elapsed_milliseconds),
        ];
        return summary_card("Dry run complete", configuration.name.as_str(), &rows);
    }

    let mut rows = vec![
        format!(
            "✓ {} complete",
            count_text(configuration.actions.len(), "action", "actions")
        ),
        format!(
            "↻ {} changed",
            count_text(result.report.changed_count, "action", "actions")
        ),
        format!(
            "• {} already correct",
            count_text(result.report.already_correct_count, "action", "actions")
        ),
        format!("⏱ Completed in {} ms", result.report.elapsed_milliseconds),
    ];
    if result.report.skipped_count > 0 {
        rows.push(format!(
            "○ {} skipped",
            count_text(result.report.skipped_count, "action", "actions")
        ));
    }
    rows.push(String::new());
    if background {
        rows.push("Background session".to_owned());
        rows.push(format!("  tmux attach-session -t workstate-{slug}"));
        rows.push(String::new());
    }
    rows.push(format!("Run again: workstate {slug}"));
    rows.push(format!("Stop with: workstate stop {slug}"));
    summary_card("Environment ready", configuration.name.as_str(), &rows)
}

fn render_stop_dry_run_summary(
    configuration: &EnvironmentConfig,
    slug: &EnvironmentSlug,
    active: bool,
) -> String {
    let active_resources = if active { "detected" } else { "not detected" };
    let rows = vec![
        format!("◌ Active resources {active_resources}"),
        String::new(),
        format!("Would run: workstate stop {slug}"),
    ];
    summary_card("Stop preview", configuration.name.as_str(), &rows)
}

fn render_stop_summary(
    configuration: &EnvironmentConfig,
    slug: &EnvironmentSlug,
    result: &StopResult,
) -> String {
    let mut rows = vec![
        format!(
            "✓ {} cleaned",
            count_text(result.cleaned_resources, "resource", "resources")
        ),
        format!(
            "• {} preserved",
            count_text(result.preserved_resources, "resource", "resources")
        ),
    ];
    if result.stale_resources > 0 {
        rows.push(format!(
            "↺ {} stale",
            count_text(result.stale_resources, "resource", "resources")
        ));
    }
    rows.push(String::new());
    rows.push(format!("Run again: workstate {slug}"));
    summary_card("Environment stopped", configuration.name.as_str(), &rows)
}

fn summary_card(title: &str, subject: &str, rows: &[String]) -> String {
    let mut body = Vec::with_capacity(rows.len() + 3);
    body.push(title.to_owned());
    body.push(subject.to_owned());
    body.push(String::new());
    body.extend(rows.iter().cloned());

    let content_width = body
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0)
        .max(44);
    let border = "─".repeat(content_width + 2);
    let mut rendered = Vec::with_capacity(body.len() + 2);
    rendered.push(format!("╭{border}╮"));
    for line in body {
        let padding = " ".repeat(content_width.saturating_sub(line.chars().count()));
        rendered.push(format!("│ {line}{padding} │"));
    }
    rendered.push(format!("╰{border}╯"));
    rendered.join("\n")
}

fn count_text(count: usize, singular: &str, plural: &str) -> String {
    let label = if count == 1 { singular } else { plural };
    format!("{count} {label}")
}

#[cfg(test)]
mod tests {
    use super::{count_text, summary_card};

    #[test]
    fn summary_card_is_compact_and_keeps_each_summary_row_visible() {
        let rendered = summary_card(
            "Environment ready",
            "notefinder",
            &[
                "✓ 4 actions complete".to_owned(),
                "Stop with: workstate stop notefinder".to_owned(),
            ],
        );

        assert!(rendered.starts_with("╭"));
        assert!(rendered.ends_with("╯"));
        assert!(rendered.contains("Environment ready"));
        assert!(rendered.contains("notefinder"));
        assert!(rendered.contains("✓ 4 actions complete"));
        assert!(rendered.contains("Stop with: workstate stop notefinder"));
    }

    #[test]
    fn count_text_uses_the_correct_singular_form() {
        assert_eq!(count_text(1, "action", "actions"), "1 action");
        assert_eq!(count_text(2, "action", "actions"), "2 actions");
    }
}
