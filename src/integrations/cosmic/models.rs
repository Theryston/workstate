use serde::Deserialize;
use serde_json::Value;

use crate::application::ports::{DesktopSnapshot, DesktopWindowSnapshot, DesktopWorkspaceSnapshot};

use super::errors::CosmicError;

#[derive(Debug, Deserialize)]
pub struct CosmicWorkspaceModel {
    pub id: Option<Value>,
    pub name: Option<String>,
    #[serde(default)]
    pub coordinates: Vec<i64>,
    #[serde(default)]
    pub active: bool,
    pub tiling: Option<CosmicTilingModel>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum CosmicTilingModel {
    Enabled(bool),
    State(String),
}

#[derive(Debug, Deserialize)]
pub struct CosmicWindowModel {
    #[serde(alias = "identifier", alias = "id")]
    pub identity: Option<Value>,
    #[serde(alias = "app_id", alias = "application")]
    pub application: Option<String>,
    pub title: Option<String>,
    #[serde(default)]
    pub state: Vec<String>,
    #[serde(default)]
    pub workspaces: Vec<Value>,
    #[serde(default)]
    pub project_path: Option<String>,
}

pub fn decode_snapshot(
    workspace_output: &[u8],
    window_output: &[u8],
) -> Result<DesktopSnapshot, CosmicError> {
    let workspaces = serde_json::from_slice::<Vec<CosmicWorkspaceModel>>(workspace_output)
        .map_err(|source| CosmicError::MalformedOutput {
            operation: "get-workspaces".to_owned(),
            detail: source.to_string(),
        })?;
    let windows =
        serde_json::from_slice::<Vec<CosmicWindowModel>>(window_output).map_err(|source| {
            CosmicError::MalformedOutput {
                operation: "get-toplevels".to_owned(),
                detail: source.to_string(),
            }
        })?;

    let mut desktop_workspaces = Vec::with_capacity(workspaces.len());
    for (position, workspace) in workspaces.into_iter().enumerate() {
        let identity = value_string(workspace.id.as_ref())
            .or_else(|| workspace.name.clone())
            .ok_or_else(|| CosmicError::IncompleteOutput {
                operation: "get-workspaces".to_owned(),
                detail: "every workspace must expose an identifier or name".to_owned(),
            })?;
        validate_text(&identity, "workspace identifier", "get-workspaces")?;
        if desktop_workspaces
            .iter()
            .any(|item: &DesktopWorkspaceSnapshot| item.identity == identity)
        {
            return Err(CosmicError::MalformedOutput {
                operation: "get-workspaces".to_owned(),
                detail: format!("workspace identifier '{identity}' appeared more than once"),
            });
        }
        if let Some(name) = &workspace.name {
            validate_text(name, "workspace name", "get-workspaces")?;
        }
        let name = workspace.name.filter(|value| !value.is_empty());
        let tiling_enabled = workspace.tiling.map(tiling_value).transpose()?;
        desktop_workspaces.push(DesktopWorkspaceSnapshot {
            identity,
            name,
            position: workspace
                .coordinates
                .first()
                .and_then(|value| u32::try_from(*value).ok())
                .or_else(|| u32::try_from(position).ok()),
            focused: workspace.active,
            tiling_enabled,
        });
    }

    if desktop_workspaces.is_empty() {
        return Err(CosmicError::IncompleteOutput {
            operation: "get-workspaces".to_owned(),
            detail: "the compositor returned no workspaces".to_owned(),
        });
    }

    let workspace_ids = desktop_workspaces
        .iter()
        .map(|workspace| workspace.identity.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let mut desktop_windows = Vec::with_capacity(windows.len());
    for window in windows {
        let identity = value_string(window.identity.as_ref()).ok_or_else(|| {
            CosmicError::IncompleteOutput {
                operation: "get-toplevels".to_owned(),
                detail: "every window must expose a stable identifier".to_owned(),
            }
        })?;
        validate_text(&identity, "window identifier", "get-toplevels")?;
        if desktop_windows
            .iter()
            .any(|item: &DesktopWindowSnapshot| item.identity == identity)
        {
            return Err(CosmicError::MalformedOutput {
                operation: "get-toplevels".to_owned(),
                detail: format!("window identifier '{identity}' appeared more than once"),
            });
        }
        if let Some(application) = &window.application {
            validate_text(application, "window application", "get-toplevels")?;
        }
        if let Some(title) = &window.title {
            validate_text(title, "window title", "get-toplevels")?;
        }
        for state in &window.state {
            validate_text(state, "window state", "get-toplevels")?;
        }
        let window_identity = identity.clone();
        let workspace_identities = window
            .workspaces
            .iter()
            .map(|value| {
                let identity = value_string(Some(value)).ok_or_else(|| {
                    CosmicError::IncompleteOutput {
                        operation: "get-toplevels".to_owned(),
                        detail: format!(
                            "window '{window_identity}' contains a workspace without a stable identifier"
                        ),
                    }
                })?;
                validate_text(&identity, "workspace identifier", "get-toplevels")?;
                if !workspace_ids.contains(identity.as_str()) {
                    return Err(CosmicError::MalformedOutput {
                        operation: "get-toplevels".to_owned(),
                        detail: format!(
                            "window '{window_identity}' references unknown workspace '{identity}'"
                        ),
                    });
                }
                Ok(identity)
            })
            .collect::<Result<Vec<_>, CosmicError>>()?;
        let workspace_identity = match workspace_identities.as_slice() {
            [identity] => Some(identity.clone()),
            _ => None,
        };
        if let Some(path) = &window.project_path {
            validate_text(path, "project path", "get-toplevels")?;
        }
        let focused = window
            .state
            .iter()
            .any(|state| state.eq_ignore_ascii_case("activated"));
        desktop_windows.push(DesktopWindowSnapshot {
            identity,
            application: window.application,
            title: window.title,
            project_path: window.project_path,
            workspace_identity,
            focused,
        });
    }

    Ok(DesktopSnapshot {
        workspaces: desktop_workspaces,
        windows: desktop_windows,
    })
}

fn value_string(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(value)) => Some(value.clone()),
        Some(Value::Number(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn tiling_value(value: CosmicTilingModel) -> Result<bool, CosmicError> {
    match value {
        CosmicTilingModel::Enabled(value) => Ok(value),
        CosmicTilingModel::State(value) if value.eq_ignore_ascii_case("enabled") => Ok(true),
        CosmicTilingModel::State(value) if value.eq_ignore_ascii_case("disabled") => Ok(false),
        CosmicTilingModel::State(value) => Err(CosmicError::MalformedOutput {
            operation: "get-workspaces".to_owned(),
            detail: format!("unsupported tiling state '{value}'"),
        }),
    }
}

fn validate_text(value: &str, field: &str, operation: &str) -> Result<(), CosmicError> {
    if value.is_empty() || value.contains('\0') || value.chars().any(char::is_control) {
        return Err(CosmicError::MalformedOutput {
            operation: operation.to_owned(),
            detail: format!("{field} must be non-empty and contain no control characters"),
        });
    }
    Ok(())
}
