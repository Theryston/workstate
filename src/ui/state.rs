use std::fmt::{self, Display, Formatter};

use crossterm::event::KeyCode;

use crate::domain::{EnvironmentName, EnvironmentSlug, RunStatus, RuntimeState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvironmentStatus {
    Ready,
    Partial,
    Stopped,
    Unknown,
}

impl EnvironmentStatus {
    pub fn from_runtime(state: Option<&RuntimeState>) -> Self {
        let Some(state) = state else {
            return Self::Unknown;
        };

        match &state.status {
            RunStatus::Ready => Self::Ready,
            RunStatus::Stopped => Self::Stopped,
            RunStatus::Active
            | RunStatus::Planning
            | RunStatus::Partial
            | RunStatus::RollingBack
            | RunStatus::RollbackFailed
            | RunStatus::Stopping
            | RunStatus::Deleting => Self::Partial,
        }
    }
}

impl Display for EnvironmentStatus {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Ready => "ready",
            Self::Partial => "partial",
            Self::Stopped => "stopped",
            Self::Unknown => "unknown",
        };
        formatter.write_str(label)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentListItem {
    pub name: EnvironmentName,
    pub slug: EnvironmentSlug,
    pub status: EnvironmentStatus,
}

impl EnvironmentListItem {
    pub fn new(name: EnvironmentName, slug: EnvironmentSlug, status: EnvironmentStatus) -> Self {
        Self { name, slug, status }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorAction {
    None,
    Cancel,
    Selected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectorState {
    items: Vec<EnvironmentListItem>,
    selected: Option<usize>,
}

impl SelectorState {
    pub fn new(mut items: Vec<EnvironmentListItem>) -> Self {
        items.sort_by(|left, right| left.slug.cmp(&right.slug));
        let selected = (!items.is_empty()).then_some(0);
        Self { items, selected }
    }

    pub fn items(&self) -> &[EnvironmentListItem] {
        &self.items
    }

    pub fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    pub fn selected_item(&self) -> Option<&EnvironmentListItem> {
        self.selected.and_then(|index| self.items.get(index))
    }

    pub fn selected_slug(&self) -> Option<EnvironmentSlug> {
        self.selected_item().map(|item| item.slug.clone())
    }

    pub fn empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn move_up(&mut self) {
        self.move_selection(-1);
    }

    pub fn move_down(&mut self) {
        self.move_selection(1);
    }

    pub fn handle_key(&mut self, key: KeyCode) -> SelectorAction {
        match key {
            KeyCode::Up => {
                self.move_up();
                SelectorAction::None
            }
            KeyCode::Down => {
                self.move_down();
                SelectorAction::None
            }
            KeyCode::Enter if self.selected.is_some() => SelectorAction::Selected,
            KeyCode::Esc | KeyCode::Char('q') => SelectorAction::Cancel,
            _ => SelectorAction::None,
        }
    }

    fn move_selection(&mut self, offset: isize) {
        let Some(current) = self.selected else {
            return;
        };
        let length = self.items.len() as isize;
        let next = (current as isize + offset).rem_euclid(length) as usize;
        self.selected = Some(next);
    }
}

pub const SELECTOR_EMPTY_MESSAGE: &str =
    "No environments yet. Create one with: workstate new <environment>";

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;

    use crate::domain::{EnvironmentName, EnvironmentSlug};

    use super::{
        EnvironmentListItem, EnvironmentStatus, SELECTOR_EMPTY_MESSAGE, SelectorAction,
        SelectorState,
    };

    fn item(name: &str, slug: &str) -> Option<EnvironmentListItem> {
        Some(EnvironmentListItem::new(
            EnvironmentName::new(name).ok()?,
            EnvironmentSlug::new(slug).ok()?,
            EnvironmentStatus::Unknown,
        ))
    }

    #[test]
    fn empty_selector_has_an_actionable_message_and_cannot_select() {
        let mut state = SelectorState::new(Vec::new());
        assert!(state.empty());
        assert_eq!(state.selected_slug(), None);
        assert_eq!(state.handle_key(KeyCode::Enter), SelectorAction::None);
        assert!(SELECTOR_EMPTY_MESSAGE.contains("workstate new"));
    }

    #[test]
    fn selector_supports_keyboard_navigation_for_one_and_many_items() {
        let Some(first) = item("Alpha", "alpha") else {
            return;
        };
        let one = SelectorState::new(vec![first]);
        assert_eq!(one.selected_index(), Some(0));

        let Some(second) = item("Beta", "beta") else {
            return;
        };
        let Some(alpha) = item("Alpha", "alpha") else {
            return;
        };
        let mut many = SelectorState::new(vec![second, alpha]);
        assert_eq!(many.items()[0].slug.as_str(), "alpha");
        assert_eq!(many.handle_key(KeyCode::Down), SelectorAction::None);
        assert_eq!(
            many.selected_slug().map(|slug| slug.to_string()),
            Some("beta".to_owned())
        );
        assert_eq!(many.handle_key(KeyCode::Enter), SelectorAction::Selected);
        assert_eq!(many.handle_key(KeyCode::Esc), SelectorAction::Cancel);
    }
}
