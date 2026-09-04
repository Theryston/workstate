pub mod args;
pub mod command;
pub mod output;

use std::{ffi::OsString, sync::Arc};

use crate::{
    application::context::AppContext,
    application::{reconciliation::InMemoryEventSink, use_cases},
    domain::{EnvironmentConfig, EnvironmentName, EnvironmentSlug, ExecutionMode},
    error::{ErrorCategory, Result, WorkstateError},
    infrastructure::persistence::WorkstatePaths,
    ui::{
        EditorMode, EditorOutcome, EditorState, EnvironmentListItem, EnvironmentStatus,
        SelectorState, confirm_delete, edit_environment, select_environment,
    },
};

use self::{
    command::{Command, Invocation, parse_from},
    output::{ConsoleOutput, OutputPolicy, OutputSink},
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
        Command::Add { environment } => {
            add_environment(
                context,
                environment.as_str(),
                &invocation.options,
                &policy,
                &mut output,
            )
            .await
        }
        Command::Stop { environment } => {
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
    let events = Arc::new(InMemoryEventSink::default());
    let result = use_cases::run::execute(context, slug, options.dry_run, events).await?;
    let message = if options.dry_run {
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
    };
    policy.write_message(output, &message)
}

async fn add_environment(
    context: &AppContext,
    argument: &str,
    options: &args::GlobalOptions,
    policy: &OutputPolicy,
    output: &mut dyn OutputSink,
) -> Result<()> {
    if options.dry_run {
        let slug = resolve_environment_slug(argument)?;
        return policy.write_message(
            output,
            &format!("Dry run: the editor would create or edit '{slug}'."),
        );
    }

    let slug = resolve_environment_slug(argument)?;
    let existing = context.config_store().load(&slug)?;
    let (configuration, mode) = match existing {
        Some(configuration) => (configuration, EditorMode::Edit),
        None => (
            EnvironmentConfig::new(argument).map_err(WorkstateError::from)?,
            EditorMode::Create,
        ),
    };
    let editor = match context.desktop_backend().snapshot().await {
        Ok(snapshot) => {
            EditorState::new(configuration, mode).with_live_workspaces(snapshot.workspaces)
        }
        Err(error) => {
            EditorState::new(configuration, mode).with_workspace_observation_error(error.render())
        }
    };
    match edit_environment(editor, options.no_color)? {
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
        return policy.write_message(
            output,
            &format!(
                "Dry run: '{}' would be stopped. Active resources detected: {active}.",
                configuration.name
            ),
        );
    }
    let events = Arc::new(InMemoryEventSink::default());
    let result = use_cases::stop::execute(context, &slug, events).await?;
    policy.write_message(
        output,
        &format!(
            "Environment '{}' stopped. Cleaned {} resource(s); preserved {}.",
            configuration.name, result.cleaned_resources, result.preserved_resources
        ),
    )
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

fn environment_not_found(argument: &str) -> WorkstateError {
    WorkstateError::new(
        ErrorCategory::Persistence,
        format!("environment '{argument}' was not found"),
    )
    .with_context("suggested_command", format!("workstate add {argument}"))
}
