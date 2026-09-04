use dialoguer::{Input, theme::{ColorfulTheme, SimpleTheme, Theme as DialoguerTheme}};

use crate::error::{ErrorCategory, Result, WorkstateError};

pub fn text<V>(
    prompt: &str,
    initial: Option<&str>,
    no_color: bool,
    validator: V,
) -> Result<Option<String>>
where
    V: FnMut(&String) -> std::result::Result<(), String>,
{
    let colorful_theme = ColorfulTheme::default();
    let simple_theme = SimpleTheme;
    let theme: &dyn DialoguerTheme = if no_color {
        &simple_theme
    } else {
        &colorful_theme
    };
    let mut input = Input::<String>::with_theme(theme).with_prompt(prompt);
    if let Some(initial) = initial {
        input = input.with_initial_text(initial);
    }
    match input.validate_with(validator).interact_text() {
        Ok(value) => Ok(Some(value)),
        Err(dialoguer::Error::IO(source)) if source.kind() == std::io::ErrorKind::Interrupted => {
            Ok(None)
        }
        Err(dialoguer::Error::IO(source)) => Err(WorkstateError::with_source(
            ErrorCategory::Ui,
            format!("could not read the {prompt} input"),
            source,
        )),
    }
}
