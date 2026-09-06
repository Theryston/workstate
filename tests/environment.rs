use std::{
    collections::BTreeMap,
    error::Error,
    sync::{Arc, Mutex},
};

use tempfile::tempdir;
use workstate::{
    AppContext, AppDependencies,
    application::{
        planner::{
            ActionHandler, ActionOutput, ActionOutputSink, ActionOutputStream, CancellationToken,
        },
        ports::{BoxFuture, EnvironmentLifecycleBackend},
        reconciliation::{InMemoryEventSink, RunRequest, SchedulerOptions},
    },
    domain::{
        ActionKind, ActionSpec, EnvironmentConfig, EnvironmentSlug, OwnershipStatus, ResourceKind,
    },
    error::{ErrorCategory, Result, WorkstateError},
    infrastructure::filesystem::local::LocalFileSystem,
    integrations::StartOtherEnvironmentActionHandler,
};

type TestResult = std::result::Result<(), Box<dyn Error>>;

#[derive(Clone, Default)]
struct RecordingOutput {
    messages: Arc<Mutex<Vec<ActionOutput>>>,
}

impl RecordingOutput {
    fn snapshot(&self) -> Result<Vec<ActionOutput>> {
        self.messages
            .lock()
            .map(|messages| messages.clone())
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "output lock failed"))
    }
}

impl ActionOutputSink for RecordingOutput {
    fn emit<'a>(&'a self, output: ActionOutput) -> BoxFuture<'a, Result<()>> {
        let result = self
            .messages
            .lock()
            .map(|mut messages| messages.push(output))
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "output lock failed"));
        Box::pin(async move { result })
    }
}

#[derive(Clone, Default)]
struct FakeEnvironmentBackend {
    states: Arc<Mutex<BTreeMap<EnvironmentSlug, bool>>>,
}

impl FakeEnvironmentBackend {
    fn set_environment(&self, environment: EnvironmentSlug, active: bool) -> Result<()> {
        self.states
            .lock()
            .map(|mut states| {
                states.insert(environment, active);
            })
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "state lock failed"))
    }

    fn active(&self, environment: &EnvironmentSlug) -> Result<bool> {
        self.states
            .lock()
            .map(|states| states.get(environment).copied().unwrap_or(false))
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "state lock failed"))
    }
}

impl EnvironmentLifecycleBackend for FakeEnvironmentBackend {
    fn exists(&self, environment: &EnvironmentSlug) -> Result<bool> {
        self.states
            .lock()
            .map(|states| states.contains_key(environment))
            .map_err(|_| WorkstateError::new(ErrorCategory::Runtime, "state lock failed"))
    }

    fn is_active(&self, environment: &EnvironmentSlug) -> Result<bool> {
        self.active(environment)
    }

    fn start<'a>(
        &'a self,
        environment: &'a EnvironmentSlug,
        cancellation: CancellationToken,
        output: Arc<dyn ActionOutputSink>,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            cancellation.check()?;
            let was_active = self.active(environment)?;
            self.set_environment(environment.clone(), true)?;
            output
                .emit(ActionOutput::log(format!(
                    "nested environment '{environment}' ready"
                )))
                .await?;
            Ok(!was_active)
        })
    }

    fn stop<'a>(
        &'a self,
        environment: &'a EnvironmentSlug,
        cancellation: CancellationToken,
        output: Arc<dyn ActionOutputSink>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            cancellation.check()?;
            self.set_environment(environment.clone(), false)?;
            output
                .emit(ActionOutput::log(format!(
                    "nested environment '{environment}' stopped"
                )))
                .await?;
            Ok(())
        })
    }
}

fn action() -> Result<ActionSpec> {
    let parent = EnvironmentConfig::new("Orchestrator")?;
    let target = EnvironmentSlug::new("api")?;
    let mut action = ActionSpec::new("start-api", ActionKind::StartOtherEnvironment)
        .map_err(WorkstateError::from)?;
    action.parameters.other_environment = Some(target);
    action.resolved_environment = Some(parent.slug);
    Ok(action)
}

#[tokio::test]
async fn start_other_environment_starts_and_stops_the_selected_environment() -> TestResult {
    let backend = Arc::new(FakeEnvironmentBackend::default());
    let target = EnvironmentSlug::new("api")?;
    backend.set_environment(target, false)?;
    let handler = StartOtherEnvironmentActionHandler::new(backend.clone());
    let action = action()?;
    handler.validate(&action)?;

    let output = RecordingOutput::default();
    let started = handler
        .run_once_with_output(&action, CancellationToken::new(), Arc::new(output.clone()))
        .await?;
    assert!(started.changed);
    assert_eq!(started.resources.len(), 1);
    assert_eq!(
        started.resources[0].resource.kind,
        ResourceKind::Environment
    );
    assert_eq!(
        started.resources[0].ownership,
        OwnershipStatus::CreatedByCurrentRun
    );
    assert!(backend.active(&EnvironmentSlug::new("api")?)?);

    let messages = output.snapshot()?;
    assert!(messages.iter().any(|message| {
        message.stream == ActionOutputStream::Log
            && message.message.contains("Starting other environment 'api'")
    }));
    assert!(
        messages
            .iter()
            .any(|message| { message.message.contains("nested environment 'api' ready") })
    );

    let stopped = handler
        .stop(&action, &started.resources, CancellationToken::new())
        .await?;
    assert!(!backend.active(&EnvironmentSlug::new("api")?)?);
    assert!(
        stopped
            .outputs
            .iter()
            .any(|message| message.message.contains("Stopping other environment 'api'"))
    );
    Ok(())
}

#[tokio::test]
async fn already_active_environment_is_reused_without_claiming_cleanup() -> TestResult {
    let backend = Arc::new(FakeEnvironmentBackend::default());
    let target = EnvironmentSlug::new("api")?;
    backend.set_environment(target, true)?;
    let handler = StartOtherEnvironmentActionHandler::new(backend);
    let action = action()?;

    let observation = handler.observe(&action, CancellationToken::new()).await?;
    assert_eq!(
        observation.status,
        workstate::application::planner::ObservationStatus::AlreadyCorrect
    );
    assert_eq!(
        observation.resources[0].ownership,
        OwnershipStatus::ReusedExisting
    );
    Ok(())
}

#[tokio::test]
async fn context_runs_and_stops_a_nested_environment_through_the_lifecycle_engine() -> TestResult {
    let root = tempdir()?;
    let mut dependencies = AppDependencies::with_noop_dependencies();
    dependencies.file_system = Arc::new(LocalFileSystem);
    let context = AppContext::new(dependencies).with_config_root(root.path().to_path_buf())?;

    let target = EnvironmentConfig::new("API")?;
    context.config_store().create(&target)?;
    let mut parent = EnvironmentConfig::new("Orchestrator")?;
    let mut nested_action = ActionSpec::new("start-api", ActionKind::StartOtherEnvironment)
        .map_err(WorkstateError::from)?;
    nested_action.parameters.other_environment = Some(target.slug.clone());
    parent
        .add_action(nested_action)
        .map_err(WorkstateError::from)?;
    context.config_store().create(&parent)?;

    let events = Arc::new(InMemoryEventSink::default());
    let engine = context.lifecycle_engine(SchedulerOptions::default())?;
    engine
        .run(
            &parent,
            RunRequest::new("parent-run", false)?,
            CancellationToken::new(),
            events.clone(),
        )
        .await?;
    assert_eq!(
        context
            .state_store()
            .load(&target.slug)?
            .map(|state| state.status),
        Some(workstate::domain::RunStatus::Ready)
    );

    engine
        .stop(&parent, CancellationToken::new(), events.clone())
        .await?;
    assert!(context.state_store().load(&target.slug)?.is_none());
    assert!(context.state_store().load(&parent.slug)?.is_none());
    assert!(events.snapshot()?.iter().any(|event| {
        matches!(
            event,
            workstate::application::reconciliation::ApplicationEvent::ActionOutput {
                message,
                ..
            } if message.contains("Stopping other environment 'api'")
        )
    }));
    Ok(())
}
