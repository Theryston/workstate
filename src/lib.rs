pub mod application;
pub mod domain;
pub mod error;
pub mod infrastructure;
pub mod integrations;
pub mod platform;
pub mod ui;

pub mod cli;

pub use application::context::{AppContext, AppDependencies};
pub use error::{ErrorCategory, Result, WorkstateError};

pub async fn run(context: AppContext) -> Result<()> {
    cli::run(context).await
}

pub async fn run_with_args(context: AppContext, arguments: Vec<std::ffi::OsString>) -> Result<()> {
    cli::run_with_args(context, arguments).await
}

pub fn meta_output(arguments: Vec<std::ffi::OsString>) -> Option<String> {
    cli::command::meta_output(arguments)
}

pub fn render_error_for_args(arguments: &[std::ffi::OsString], error: &WorkstateError) -> String {
    cli::render_error_for_args(arguments, error)
}

#[cfg(test)]
mod tests {
    use super::AppContext;

    #[tokio::test]
    async fn library_runner_accepts_placeholder_context_without_external_side_effects() {
        let context = AppContext::with_noop_dependencies();
        let result = context.preflight();

        assert!(result.is_ok());
    }
}
