use std::sync::Arc;

use crate::{
    application::{
        context::AppContext,
        reconciliation::{EventSink, LifecycleRunResult, RunRequest, SchedulerOptions},
        use_cases::load_environment,
    },
    domain::EnvironmentSlug,
    error::Result,
};

pub async fn execute(
    context: &AppContext,
    environment: &EnvironmentSlug,
    dry_run: bool,
    events: Arc<dyn EventSink>,
) -> Result<LifecycleRunResult> {
    context.preflight()?;
    let configuration = load_environment(context, environment)?;
    let request = RunRequest::new(generate_run_id(context), dry_run)?;
    context
        .lifecycle_engine(SchedulerOptions::default())
        .run(&configuration, request, Default::default(), events)
        .await
}

fn generate_run_id(context: &AppContext) -> String {
    let timestamp = match context.clock().now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos(),
        Err(_) => 0,
    };
    format!("run-{timestamp}-{}", std::process::id())
}
