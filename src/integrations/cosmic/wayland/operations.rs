use cosmic_client_toolkit::{toplevel_info::ToplevelInfo, workspace::Workspace};
use cosmic_protocols::workspace::v2::client::zcosmic_workspace_handle_v2;
use wayland_client::WEnum;

use super::super::errors::CosmicError;
use super::{snapshot::protocol_workspace_identity, state::CosmicMutationState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IdentityResolution {
    Missing,
    Unique,
    Ambiguous(usize),
}

pub(crate) fn classify_identity_matches<'a, I>(identities: I, requested: &str) -> IdentityResolution
where
    I: IntoIterator<Item = &'a str>,
{
    let matches = identities
        .into_iter()
        .filter(|identity| *identity == requested)
        .count();
    match matches {
        0 => IdentityResolution::Missing,
        1 => IdentityResolution::Unique,
        matches => IdentityResolution::Ambiguous(matches),
    }
}

pub(crate) fn resolve_workspace<'a>(
    state: &'a CosmicMutationState,
    identity: &str,
    operation: &str,
) -> Result<&'a Workspace, CosmicError> {
    let mut candidates = Vec::new();
    for workspace in state.read.workspace_state.workspaces() {
        let candidate = protocol_workspace_identity(workspace, operation)?;
        candidates.push((workspace, candidate));
    }
    match classify_identity_matches(
        candidates.iter().map(|(_, candidate)| candidate.as_str()),
        identity,
    ) {
        IdentityResolution::Unique => candidates
            .into_iter()
            .find(|(_, candidate)| candidate == identity)
            .map(|(workspace, _)| workspace)
            .ok_or_else(|| CosmicError::WorkspaceNotFound {
                operation: operation.to_owned(),
                identity: identity.to_owned(),
            }),
        IdentityResolution::Missing => Err(CosmicError::WorkspaceNotFound {
            operation: operation.to_owned(),
            identity: identity.to_owned(),
        }),
        IdentityResolution::Ambiguous(matches) => Err(CosmicError::WorkspaceAmbiguous {
            operation: operation.to_owned(),
            identity: identity.to_owned(),
            matches,
        }),
    }
}

pub(crate) fn resolve_window<'a>(
    state: &'a CosmicMutationState,
    identity: &str,
    operation: &str,
) -> Result<&'a ToplevelInfo, CosmicError> {
    let candidates = state
        .read
        .toplevel_info_state
        .toplevels()
        .collect::<Vec<_>>();
    match classify_identity_matches(
        candidates
            .iter()
            .map(|toplevel| toplevel.identifier.as_str()),
        identity,
    ) {
        IdentityResolution::Unique => candidates
            .into_iter()
            .find(|toplevel| toplevel.identifier == identity)
            .ok_or_else(|| CosmicError::WindowNotFound {
                operation: operation.to_owned(),
                identity: identity.to_owned(),
            }),
        IdentityResolution::Missing => Err(CosmicError::WindowNotFound {
            operation: operation.to_owned(),
            identity: identity.to_owned(),
        }),
        IdentityResolution::Ambiguous(matches) => Err(CosmicError::WindowAmbiguous {
            operation: operation.to_owned(),
            identity: identity.to_owned(),
            matches,
        }),
    }
}

pub(crate) fn target_output(
    state: &CosmicMutationState,
    workspace: &Workspace,
    operation: &str,
    workspace_identity: &str,
) -> Result<wayland_client::protocol::wl_output::WlOutput, CosmicError> {
    let group = state
        .read
        .workspace_state
        .workspace_groups()
        .find(|group| group.workspaces.contains(&workspace.handle));
    let Some(group) = group else {
        return Err(CosmicError::TargetWorkspaceOutputMissing {
            operation: operation.to_owned(),
            workspace: workspace_identity.to_owned(),
        });
    };
    group
        .outputs
        .first()
        .cloned()
        .ok_or_else(|| CosmicError::TargetWorkspaceOutputMissing {
            operation: operation.to_owned(),
            workspace: workspace_identity.to_owned(),
        })
}

pub(crate) fn workspace_tiling_enabled(workspace: &Workspace) -> Option<bool> {
    match workspace.tiling.as_ref() {
        Some(WEnum::Value(zcosmic_workspace_handle_v2::TilingState::TilingEnabled)) => Some(true),
        Some(WEnum::Value(zcosmic_workspace_handle_v2::TilingState::FloatingOnly)) => Some(false),
        Some(WEnum::Value(_)) | Some(WEnum::Unknown(_)) | None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{IdentityResolution, classify_identity_matches};

    #[test]
    fn classifies_missing_and_unique_identities_exactly() {
        assert_eq!(
            classify_identity_matches(["main", "secondary"], "missing"),
            IdentityResolution::Missing
        );
        assert_eq!(
            classify_identity_matches(["main", "secondary"], "main"),
            IdentityResolution::Unique
        );
    }

    #[test]
    fn refuses_ambiguous_identities_instead_of_picking_one() {
        assert_eq!(
            classify_identity_matches(["main", "main"], "main"),
            IdentityResolution::Ambiguous(2)
        );
    }
}
