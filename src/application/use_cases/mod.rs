use crate::{
    application::context::AppContext,
    domain::{EnvironmentConfig, EnvironmentSlug},
    error::{ErrorCategory, Result, WorkstateError},
};

pub mod delete;
pub mod run;
pub mod stop;

pub(crate) fn load_environment(
    context: &AppContext,
    environment: &EnvironmentSlug,
) -> Result<EnvironmentConfig> {
    let Some(configuration) = context.config_store().load(environment)? else {
        return Err(environment_not_found(environment.as_str()));
    };
    configuration.validate().map_err(WorkstateError::from)?;
    Ok(configuration)
}

pub(crate) fn environment_not_found(argument: &str) -> WorkstateError {
    WorkstateError::new(
        ErrorCategory::Persistence,
        format!("environment '{argument}' was not found"),
    )
    .with_context("suggested_command", format!("workstate new {argument}"))
}
