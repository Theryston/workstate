pub mod application;
pub mod domain;
pub mod error;
pub mod infrastructure;
pub mod integrations;
pub mod platform;
pub mod ui;

mod cli;

pub use application::context::{AppContext, AppDependencies};
pub use error::{ErrorCategory, Result, WorkstateError};

pub async fn run(context: AppContext) -> Result<()> {
    cli::run(context).await
}

#[cfg(test)]
mod tests {
    use super::{AppContext, run};

    #[tokio::test]
    async fn library_runner_accepts_placeholder_context_without_external_side_effects() {
        let context = AppContext::with_noop_dependencies();
        let result = run(context).await;

        assert!(result.is_ok());
    }
}
