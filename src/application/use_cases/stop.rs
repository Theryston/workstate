use std::sync::Arc;

use crate::{
    application::{
        context::AppContext,
        reconciliation::{EventSink, SchedulerOptions, StopResult},
        use_cases::load_environment,
    },
    domain::EnvironmentSlug,
    error::Result,
};

pub async fn execute(
    context: &AppContext,
    environment: &EnvironmentSlug,
    events: Arc<dyn EventSink>,
) -> Result<StopResult> {
    context.preflight()?;
    let configuration = load_environment(context, environment)?;
    context
        .lifecycle_engine(SchedulerOptions::default())?
        .stop(&configuration, Default::default(), events)
        .await
}
