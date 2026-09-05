use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs, io,
    path::{Path, PathBuf},
};

use crate::{
    application::ports::{ApplicationCatalog, ApplicationLaunchSpec, InstalledApplication},
    domain::{ActionId, CommandSpec},
    error::{ErrorCategory, Result, WorkstateError},
};

#[derive(Debug, Clone)]
pub struct LinuxApplicationCatalog {
    directories: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
struct ParsedDesktopEntry {
    application: InstalledApplication,
    launch: Option<ApplicationLaunchSpec>,
}

impl LinuxApplicationCatalog {
    pub fn new() -> Self {
        Self {
            directories: default_application_directories(),
        }
    }

    pub fn with_directories(directories: Vec<PathBuf>) -> Self {
        Self { directories }
    }
}

impl Default for LinuxApplicationCatalog {
    fn default() -> Self {
        Self::new()
    }
}

impl ApplicationCatalog for LinuxApplicationCatalog {
    fn list(&self) -> Result<Vec<InstalledApplication>> {
        let mut applications = BTreeMap::new();
        for directory in &self.directories {
            for path in desktop_files(directory)? {
                let Some(parsed) = parse_desktop_entry(&path)? else {
                    continue;
                };
                applications
                    .entry(parsed.application.id.clone())
                    .or_insert(parsed.application);
            }
        }

        let mut applications = applications.into_values().collect::<Vec<_>>();
        applications.sort_by(|left, right| {
            left.name
                .to_ascii_lowercase()
                .cmp(&right.name.to_ascii_lowercase())
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(applications)
    }

    fn launch_spec(&self, application_id: &str) -> Result<ApplicationLaunchSpec> {
        let mut found = false;
        for directory in &self.directories {
            for path in desktop_files(directory)? {
                let Some(parsed) = parse_desktop_entry(&path)? else {
                    continue;
                };
                if parsed.application.id == application_id {
                    found = true;
                    if let Some(launch) = parsed.launch {
                        return Ok(launch);
                    }
                }
            }
        }

        Err(WorkstateError::new(
            ErrorCategory::Platform,
            if found {
                format!(
                    "installed application '{application_id}' does not expose a safe executable launch command"
                )
            } else {
                format!(
                    "installed application '{application_id}' could not be resolved to a launch command"
                )
            },
        )
        .with_context("application_id", application_id.to_owned()))
    }
}

fn default_application_directories() -> Vec<PathBuf> {
    let mut directories = Vec::new();
    if let Some(data_home) = non_empty_environment("XDG_DATA_HOME") {
        directories.push(PathBuf::from(data_home).join("applications"));
    } else if let Some(home) = non_empty_environment("HOME") {
        directories.push(PathBuf::from(home).join(".local/share/applications"));
    }

    let data_directories = non_empty_environment("XDG_DATA_DIRS")
        .map(|value| env::split_paths(&value).collect::<Vec<_>>())
        .filter(|paths| !paths.is_empty())
        .unwrap_or_else(|| {
            vec![
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share"),
            ]
        });
    directories.extend(
        data_directories
            .into_iter()
            .map(|directory| directory.join("applications")),
    );

    if let Some(home) = non_empty_environment("HOME") {
        directories
            .push(PathBuf::from(home).join(".local/share/flatpak/exports/share/applications"));
    }
    directories.push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));
    directories.push(PathBuf::from("/var/lib/snapd/desktop/applications"));

    let mut unique = BTreeSet::new();
    directories
        .into_iter()
        .filter(|directory| unique.insert(directory.clone()))
        .collect()
}

fn non_empty_environment(name: &str) -> Option<std::ffi::OsString> {
    env::var_os(name).filter(|value| !value.to_string_lossy().trim().is_empty())
}

fn desktop_files(directory: &Path) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(io_error("listing application entries", directory, error)),
    };

    let mut files = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| io_error("reading application entry", directory, error))?;
        let file_type = entry
            .file_type()
            .map_err(|error| io_error("reading application entry type", directory, error))?;
        if file_type.is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "desktop")
        {
            files.push(entry.path());
        }
    }
    files.sort();
    Ok(files)
}

fn parse_desktop_entry(path: &Path) -> Result<Option<ParsedDesktopEntry>> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound
                    | io::ErrorKind::InvalidData
                    | io::ErrorKind::PermissionDenied
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(io_error("reading application metadata", path, error)),
    };

    Ok(parse_desktop_entry_contents_with_launch(&contents, path))
}

fn parse_desktop_entry_contents_with_launch(
    contents: &str,
    path: &Path,
) -> Option<ParsedDesktopEntry> {
    let mut in_desktop_entry = false;
    let mut application_type = false;
    let mut hidden = false;
    let mut no_display = false;
    let mut dbus_activatable = false;
    let mut name = None;
    let mut exec = None;

    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_desktop_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_desktop_entry || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "Type" => application_type = value.eq_ignore_ascii_case("Application"),
            "Name" => {
                if !value.is_empty() {
                    name = Some(value.to_owned());
                }
            }
            "Hidden" => hidden = is_true(value),
            "NoDisplay" => no_display = is_true(value),
            "DBusActivatable" => dbus_activatable = is_true(value),
            "Exec" => exec = (!value.is_empty()).then(|| value.to_owned()),
            _ => {}
        }
    }

    if !application_type || hidden || no_display || (exec.is_none() && !dbus_activatable) {
        return None;
    }
    let name = name?;
    if name.chars().any(char::is_control) {
        return None;
    }
    let file_name = path.file_name().and_then(|value| value.to_str())?;
    let id = file_name.strip_suffix(".desktop")?;
    if id.is_empty() || id.chars().any(char::is_control) {
        return None;
    }

    let launch = exec.as_deref().and_then(parse_exec_line);
    Some(ParsedDesktopEntry {
        application: InstalledApplication {
            id: id.to_owned(),
            name,
        },
        launch,
    })
}

fn parse_exec_line(line: &str) -> Option<ApplicationLaunchSpec> {
    let action_id = ActionId::new("desktop-entry").ok()?;
    let command = CommandSpec::from_argv_line(&action_id, line).ok()?;
    let program = expand_exec_token(&command.program)??;
    if program.is_empty() {
        return None;
    }
    let mut arguments = Vec::new();
    for argument in command.arguments {
        if let Some(argument) = expand_exec_token(&argument)? {
            arguments.push(argument);
        }
    }
    Some(ApplicationLaunchSpec { program, arguments })
}

fn expand_exec_token(token: &str) -> Option<Option<String>> {
    let mut expanded = String::with_capacity(token.len());
    let mut field_code = false;
    let mut characters = token.chars();
    while let Some(character) = characters.next() {
        if character != '%' {
            expanded.push(character);
            continue;
        }
        let code = characters.next()?;
        match code {
            '%' => expanded.push('%'),
            'f' | 'F' | 'u' | 'U' | 'i' | 'c' | 'k' => field_code = true,
            _ => return None,
        }
    }
    if field_code && expanded.is_empty() {
        return Some(None);
    }
    Some(Some(expanded))
}

fn is_true(value: &str) -> bool {
    matches!(value.to_ascii_lowercase().as_str(), "true" | "1" | "yes")
}

fn io_error(operation: &str, path: &Path, source: io::Error) -> WorkstateError {
    WorkstateError::with_source(ErrorCategory::Platform, operation, source)
        .with_context("path", path.display().to_string())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use crate::application::ports::ApplicationCatalog;

    use super::{LinuxApplicationCatalog, parse_desktop_entry_contents_with_launch};

    #[test]
    fn parses_a_visible_launchable_application() {
        let contents = "[Desktop Entry]\nType=Application\nName=Editor\nExec=editor\n";
        let parsed = parse_desktop_entry_contents_with_launch(
            contents,
            Path::new("org.example.Editor.desktop"),
        );
        assert!(parsed.is_some());
        if let Some(parsed) = parsed {
            assert_eq!(parsed.application.id, "org.example.Editor");
            assert_eq!(parsed.application.name, "Editor");
        }
    }

    #[test]
    fn resolves_desktop_entry_arguments_without_field_code_values() {
        let contents = "[Desktop Entry]\nType=Application\nName=Editor\nExec=code --new-window %F \"two words\"\n";
        let parsed = parse_desktop_entry_contents_with_launch(
            contents,
            Path::new("org.example.Editor.desktop"),
        );
        assert!(parsed.is_some());
        let Some(parsed) = parsed else {
            return;
        };
        let Some(launch) = parsed.launch else {
            return;
        };
        assert_eq!(launch.program, "code");
        assert_eq!(
            launch.arguments,
            vec!["--new-window".to_owned(), "two words".to_owned()]
        );
    }

    #[test]
    fn catalog_resolves_a_launch_spec_for_a_selected_application() {
        let Ok(directory) = tempfile::tempdir() else {
            return;
        };
        let path = directory.path().join("browser.desktop");
        if fs::write(
            &path,
            "[Desktop Entry]\nType=Application\nName=Browser\nExec=browser --private-window\n",
        )
        .is_err()
        {
            return;
        }

        let catalog = LinuxApplicationCatalog::with_directories(vec![directory.path().to_owned()]);
        let launch = catalog.launch_spec("browser");
        assert!(launch.is_ok());
        let Some(launch) = launch.ok() else {
            return;
        };
        assert_eq!(launch.program, "browser");
        assert_eq!(launch.arguments, vec!["--private-window".to_owned()]);
    }

    #[test]
    fn lists_visible_applications_in_stable_display_order() {
        let Ok(directory) = tempfile::tempdir() else {
            return;
        };
        let first = directory.path().join("editor.desktop");
        let second = directory.path().join("browser.desktop");
        let hidden = directory.path().join("hidden.desktop");
        if fs::write(
            &first,
            "[Desktop Entry]\nType=Application\nName=Editor\nExec=editor\n",
        )
        .is_err()
            || fs::write(
                &second,
                "[Desktop Entry]\nType=Application\nName=Browser\nExec=browser\n",
            )
            .is_err()
            || fs::write(
                &hidden,
                "[Desktop Entry]\nType=Application\nName=Hidden\nHidden=true\nExec=hidden\n",
            )
            .is_err()
        {
            return;
        }

        let catalog = LinuxApplicationCatalog::with_directories(vec![directory.path().to_owned()]);
        let Ok(applications) = catalog.list() else {
            return;
        };
        assert_eq!(applications.len(), 2);
        assert_eq!(applications[0].name, "Browser");
        assert_eq!(applications[1].name, "Editor");
    }
}
