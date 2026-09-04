use crate::domain::{EnvironmentConfig, EnvironmentSlug, RuntimeState};
use crate::error::Result;

pub trait ConfigStore: Send + Sync {
    fn load(&self, environment: &EnvironmentSlug) -> Result<Option<EnvironmentConfig>>;
    fn create(&self, configuration: &EnvironmentConfig) -> Result<()>;
    fn save(&self, configuration: &EnvironmentConfig) -> Result<()>;
    fn delete(&self, environment: &EnvironmentSlug) -> Result<()>;
    fn list(&self) -> Result<Vec<EnvironmentSlug>>;
}

pub trait StateStore: Send + Sync {
    fn load(&self, environment: &EnvironmentSlug) -> Result<Option<RuntimeState>>;
    fn save(&self, state: &RuntimeState) -> Result<()>;
    fn delete(&self, environment: &EnvironmentSlug) -> Result<()>;

    fn save_if_changed(
        &self,
        state: &RuntimeState,
        previous: Option<&RuntimeState>,
    ) -> Result<bool> {
        if previous.is_some_and(|value| value == state) {
            return Ok(false);
        }
        self.save(state)?;
        Ok(true)
    }

    fn load_for(
        &self,
        environments: &[EnvironmentSlug],
    ) -> Result<Vec<(EnvironmentSlug, RuntimeState)>> {
        let mut states = Vec::new();
        for environment in environments {
            if let Some(state) = self.load(environment)? {
                states.push((environment.clone(), state));
            }
        }
        Ok(states)
    }
}
