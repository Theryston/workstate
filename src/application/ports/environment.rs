use std::sync::Arc;

use crate::{
    application::{
        planner::{ActionOutputSink, CancellationToken},
        ports::BoxFuture,
    },
    domain::EnvironmentSlug,
    error::Result,
};

pub trait EnvironmentLifecycleBackend: Send + Sync {
    fn exists(&self, environment: &EnvironmentSlug) -> Result<bool>;

    fn is_active(&self, environment: &EnvironmentSlug) -> Result<bool>;

    fn start<'a>(
        &'a self,
        environment: &'a EnvironmentSlug,
        cancellation: CancellationToken,
        output: Arc<dyn ActionOutputSink>,
    ) -> BoxFuture<'a, Result<bool>>;

    fn stop<'a>(
        &'a self,
        environment: &'a EnvironmentSlug,
        cancellation: CancellationToken,
        output: Arc<dyn ActionOutputSink>,
    ) -> BoxFuture<'a, Result<()>>;
}
