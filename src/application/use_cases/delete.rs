use std::sync::Arc;

use crate::{
    application::{
        context::AppContext,
        reconciliation::{DeleteResult, EventSink, SchedulerOptions},
        use_cases::load_environment,
    },
    domain::EnvironmentSlug,
    error::Result,
};

pub async fn execute(
    context: &AppContext,
    environment: &EnvironmentSlug,
    events: Arc<dyn EventSink>,
) -> Result<DeleteResult> {
    context.preflight()?;
    let configuration = load_environment(context, environment)?;
    context
        .lifecycle_engine(SchedulerOptions::default())
        .delete(&configuration, Default::default(), events)
        .await
}
