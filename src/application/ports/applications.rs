use std::path::PathBuf;

use crate::{
    application::ports::process::ProcessRequest,
    error::{ErrorCategory, Result, WorkstateError},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledApplication {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationLaunchSpec {
    pub program: String,
    pub arguments: Vec<String>,
}

impl ApplicationLaunchSpec {
    pub fn process_request(
        &self,
        arguments: Vec<String>,
        working_directory: Option<PathBuf>,
    ) -> ProcessRequest {
        let mut launch_arguments = self.arguments.clone();
        launch_arguments.extend(arguments);
        ProcessRequest {
            program: self.program.clone(),
            arguments: launch_arguments,
            working_directory,
            environment: Vec::new(),
        }
    }
}

pub trait ApplicationCatalog: Send + Sync {
    fn list(&self) -> Result<Vec<InstalledApplication>>;

    fn launch_spec(&self, application_id: &str) -> Result<ApplicationLaunchSpec> {
        Err(WorkstateError::new(
            ErrorCategory::Platform,
            format!("application '{application_id}' cannot be resolved to a launch command"),
        ))
    }
}
