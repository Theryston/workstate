use std::io::{self, Write};

use serde::Serialize;

use crate::error::{ErrorCategory, Result, WorkstateError};

use super::args::GlobalOptions;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Human,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputPolicy {
    pub format: OutputFormat,
    pub quiet: bool,
    pub color: bool,
}

impl OutputPolicy {
    pub fn from_options(options: &GlobalOptions) -> Self {
        Self {
            format: if options.json {
                OutputFormat::Json
            } else {
                OutputFormat::Human
            },
            quiet: options.quiet,
            color: !options.no_color,
        }
    }

    pub fn render_error(&self, error: &WorkstateError) -> Result<String> {
        match self.format {
            OutputFormat::Human => Ok(render_human_error(error, self.color)),
            OutputFormat::Json => serde_json::to_string_pretty(&ErrorDocument {
                category: error.category.to_string(),
                message: error.message.clone(),
                context: error.context.clone(),
            })
            .map_err(|source| {
                WorkstateError::with_source(
                    ErrorCategory::Runtime,
                    "could not serialize the error as JSON",
                    source,
                )
            }),
        }
    }

    pub fn write_error<S>(&self, sink: &mut S, error: &WorkstateError) -> Result<()>
    where
        S: OutputSink + ?Sized,
    {
        let rendered = self.render_error(error)?;
        sink.write_stderr(&rendered)
    }

    pub fn write_message<S>(&self, sink: &mut S, message: &str) -> Result<()>
    where
        S: OutputSink + ?Sized,
    {
        if self.quiet {
            return Ok(());
        }

        match self.format {
            OutputFormat::Human => sink.write_stdout(message),
            OutputFormat::Json => {
                let document = MessageDocument { message };
                let rendered = serde_json::to_string_pretty(&document).map_err(|source| {
                    WorkstateError::with_source(
                        ErrorCategory::Runtime,
                        "could not serialize the message as JSON",
                        source,
                    )
                })?;
                sink.write_stdout(&rendered)
            }
        }
    }
}

const ERROR_CARD_MAX_CONTENT_WIDTH: usize = 76;
const ERROR_CARD_MIN_CONTENT_WIDTH: usize = 44;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErrorLineStyle {
    Category,
    Command,
    Detail,
    Message,
    Section,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ErrorLine {
    text: String,
    style: Option<ErrorLineStyle>,
}

impl ErrorLine {
    fn new(text: impl Into<String>, style: Option<ErrorLineStyle>) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }
}

fn render_human_error(error: &WorkstateError, color: bool) -> String {
    let mut lines = Vec::new();
    lines.push(ErrorLine::new(
        format!(
            "{}  {}",
            error_symbol(error.category),
            error.category.to_string().to_ascii_uppercase()
        ),
        Some(ErrorLineStyle::Category),
    ));
    lines.push(ErrorLine::new(String::new(), None));
    append_wrapped(&mut lines, &error.message, ErrorLineStyle::Message, 0);

    if let Some(next_action) = error.context.get("next_action") {
        lines.push(ErrorLine::new(String::new(), None));
        lines.push(ErrorLine::new("Next step", Some(ErrorLineStyle::Section)));
        append_wrapped(&mut lines, next_action, ErrorLineStyle::Detail, 2);
    }
    if let Some(command) = error.context.get("suggested_command") {
        if !lines.iter().any(|line| line.text == "Next step") {
            lines.push(ErrorLine::new(String::new(), None));
            lines.push(ErrorLine::new("Next step", Some(ErrorLineStyle::Section)));
        }
        append_wrapped(
            &mut lines,
            &format!("$ {command}"),
            ErrorLineStyle::Command,
            2,
        );
    }

    let details = error_details(error);
    if !details.is_empty() {
        lines.push(ErrorLine::new(String::new(), None));
        lines.push(ErrorLine::new("Details", Some(ErrorLineStyle::Section)));
        for (label, value) in details {
            append_detail(&mut lines, &label, &value);
        }
    }

    let content_width = lines
        .iter()
        .map(|line| line.text.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(ERROR_CARD_MIN_CONTENT_WIDTH, ERROR_CARD_MAX_CONTENT_WIDTH);
    let top_label = "─ Workstate · Error ";
    let top_fill = "─".repeat(
        (content_width + 2)
            .saturating_sub(top_label.chars().count())
            .max(1),
    );
    let border = "─".repeat(content_width + 2);
    let mut rendered = Vec::with_capacity(lines.len() + 2);
    rendered.push(format!("╭{top_label}{top_fill}╮"));
    for line in lines {
        let padding = " ".repeat(content_width.saturating_sub(line.text.chars().count()));
        let content = style_error_line(&line, color);
        rendered.push(format!("│ {content}{padding} │"));
    }
    rendered.push(format!("╰{border}╯"));
    rendered.join("\n")
}

fn append_wrapped(lines: &mut Vec<ErrorLine>, value: &str, style: ErrorLineStyle, indent: usize) {
    let prefix = " ".repeat(indent);
    let width = ERROR_CARD_MAX_CONTENT_WIDTH.saturating_sub(indent);
    let mut appended = false;
    for source_line in value.lines() {
        let wrapped = wrap_line(source_line, width);
        for line in wrapped {
            lines.push(ErrorLine::new(format!("{prefix}{line}"), Some(style)));
            appended = true;
        }
    }
    if !appended {
        lines.push(ErrorLine::new(prefix, Some(style)));
    }
}

fn append_detail(lines: &mut Vec<ErrorLine>, label: &str, value: &str) {
    let label = format!("{label}:");
    let label_width = label.chars().count();
    let value_width = ERROR_CARD_MAX_CONTENT_WIDTH
        .saturating_sub(label_width)
        .saturating_sub(1);
    let wrapped = wrap_line(value, value_width);
    let mut first = true;
    for line in wrapped {
        let text = if first {
            format!("{label} {line}")
        } else {
            format!("{:width$} {line}", "", width = label_width)
        };
        lines.push(ErrorLine::new(text, Some(ErrorLineStyle::Detail)));
        first = false;
    }
}

fn error_details(error: &WorkstateError) -> Vec<(String, String)> {
    const PREFERRED_KEYS: [&str; 24] = [
        "environment",
        "action_id",
        "operation",
        "working_directory",
        "project_path",
        "application",
        "workspace_name",
        "workspace_identity",
        "service",
        "endpoint_kind",
        "endpoint",
        "exit_status",
        "attempts",
        "timeout_milliseconds",
        "detail",
        "stderr",
        "rollback",
        "rollback_error",
        "cleanup_errors",
        "partial_cleanup",
        "service_cleanup",
        "service_cleanup_error",
        "terminal_restore_error",
        "missing_capabilities",
    ];
    let mut keys = Vec::new();
    for key in PREFERRED_KEYS {
        if error.context.contains_key(key) {
            keys.push(key);
        }
    }
    for key in error.context.keys() {
        if key != "next_action"
            && key != "suggested_command"
            && !PREFERRED_KEYS.contains(&key.as_str())
        {
            keys.push(key.as_str());
        }
    }

    let mut details = keys
        .into_iter()
        .filter_map(|key| {
            error
                .context
                .get(key)
                .map(|value| (humanize_key(key), value.clone()))
        })
        .collect::<Vec<_>>();
    if let Some(source) = &error.source {
        let source = source.to_string();
        let duplicate = source.is_empty()
            || source == error.message
            || details.iter().any(|(_, value)| value == &source);
        if !duplicate {
            details.push(("Cause".to_owned(), source));
        }
    }
    details
}

fn wrap_line(value: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let cleaned = value
        .chars()
        .filter(|character| !character.is_control())
        .collect::<String>();
    if cleaned.trim().is_empty() {
        return vec![String::new()];
    }

    let mut lines = Vec::new();
    let mut current = String::new();
    for word in cleaned.split_whitespace() {
        let word_length = word.chars().count();
        if word_length > width {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            let mut chunk = String::new();
            for character in word.chars() {
                chunk.push(character);
                if chunk.chars().count() == width {
                    lines.push(std::mem::take(&mut chunk));
                }
            }
            current = chunk;
            continue;
        }
        let current_length = current.chars().count();
        let separator = usize::from(!current.is_empty());
        if current_length + separator + word_length > width && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn style_error_line(line: &ErrorLine, color: bool) -> String {
    if !color {
        return line.text.clone();
    }
    let code = match line.style {
        Some(ErrorLineStyle::Category) => "\x1b[1;31m",
        Some(ErrorLineStyle::Command) => "\x1b[1;32m",
        Some(ErrorLineStyle::Detail) => "\x1b[37m",
        Some(ErrorLineStyle::Message) => "\x1b[1;37m",
        Some(ErrorLineStyle::Section) => "\x1b[1;36m",
        None => "",
    };
    if code.is_empty() {
        line.text.clone()
    } else {
        format!("{code}{}\x1b[0m", line.text)
    }
}

fn error_symbol(category: ErrorCategory) -> &'static str {
    match category {
        ErrorCategory::Domain => "◇",
        ErrorCategory::Persistence => "▣",
        ErrorCategory::Platform => "◈",
        ErrorCategory::Process => "›",
        ErrorCategory::Integration => "×",
        ErrorCategory::Ui => "◌",
        ErrorCategory::Cli => "?",
        ErrorCategory::Runtime => "!",
    }
}

fn humanize_key(key: &str) -> String {
    let mut result = String::with_capacity(key.len());
    let mut uppercase_first = true;
    for character in key.chars() {
        if character == '_' {
            result.push(' ');
        } else if uppercase_first {
            result.extend(character.to_uppercase());
            uppercase_first = false;
        } else {
            result.push(character);
        }
    }
    result
}

pub trait OutputSink {
    fn write_stdout(&mut self, message: &str) -> Result<()>;
    fn write_stderr(&mut self, message: &str) -> Result<()>;
}

#[derive(Debug, Default)]
pub struct ConsoleOutput;

impl OutputSink for ConsoleOutput {
    fn write_stdout(&mut self, message: &str) -> Result<()> {
        write_to(io::stdout(), ErrorCategory::Runtime, "stdout", message)
    }

    fn write_stderr(&mut self, message: &str) -> Result<()> {
        write_to(io::stderr(), ErrorCategory::Runtime, "stderr", message)
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct BufferOutput {
    pub stdout: String,
    pub stderr: String,
}

impl OutputSink for BufferOutput {
    fn write_stdout(&mut self, message: &str) -> Result<()> {
        self.stdout.push_str(message);
        Ok(())
    }

    fn write_stderr(&mut self, message: &str) -> Result<()> {
        self.stderr.push_str(message);
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct ErrorDocument {
    category: String,
    message: String,
    context: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
struct MessageDocument<'a> {
    message: &'a str,
}

fn write_to<W>(mut writer: W, category: ErrorCategory, stream: &str, message: &str) -> Result<()>
where
    W: Write,
{
    writer.write_all(message.as_bytes()).map_err(|source| {
        WorkstateError::with_source(category, format!("could not write to {stream}"), source)
    })?;
    writer.write_all(b"\n").map_err(|source| {
        WorkstateError::with_source(category, format!("could not write to {stream}"), source)
    })?;
    writer.flush().map_err(|source| {
        WorkstateError::with_source(category, format!("could not flush {stream}"), source)
    })
}

#[cfg(test)]
mod tests {
    use crate::error::{ErrorCategory, WorkstateError};

    use super::{BufferOutput, OutputFormat, OutputPolicy, OutputSink};

    #[test]
    fn human_errors_preserve_structured_context_at_the_output_boundary() {
        let error = WorkstateError::new(ErrorCategory::Platform, "unsupported platform")
            .with_context("operating_system", "Linux");
        let policy = OutputPolicy {
            format: OutputFormat::Human,
            quiet: false,
            color: false,
        };
        let rendered = policy.render_error(&error);
        assert!(rendered.is_ok());
        let Some(rendered) = rendered.ok() else {
            return;
        };
        assert!(rendered.contains("Operating system: Linux"));
    }

    #[test]
    fn human_errors_render_as_a_structured_card_without_color() {
        let error = WorkstateError::new(ErrorCategory::Integration, "Docker Engine is unavailable")
            .with_context("action_id", "start-compose")
            .with_context("detail", "the daemon did not respond")
            .with_context("next_action", "Start Docker manually and try again");
        let policy = OutputPolicy {
            format: OutputFormat::Human,
            quiet: false,
            color: false,
        };
        let rendered = policy.render_error(&error);
        assert!(rendered.is_ok());
        let Some(rendered) = rendered.ok() else {
            return;
        };
        assert!(rendered.starts_with("╭─ Workstate · Error"));
        assert!(rendered.contains("×  INTEGRATION ERROR"));
        assert!(rendered.contains("Docker Engine is unavailable"));
        assert!(rendered.contains("Next step"));
        assert!(rendered.contains("Action id: start-compose"));
        assert!(rendered.contains("Details"));
        assert!(!rendered.contains("\x1b["));
    }

    #[test]
    fn colored_human_errors_use_terminal_emphasis() {
        let error = WorkstateError::new(ErrorCategory::Runtime, "operation failed");
        let policy = OutputPolicy {
            format: OutputFormat::Human,
            quiet: false,
            color: true,
        };
        let rendered = policy.render_error(&error);
        assert!(rendered.is_ok());
        let Some(rendered) = rendered.ok() else {
            return;
        };
        assert!(rendered.contains("\x1b[1;31m"));
        assert!(rendered.contains("\x1b[1;37m"));
        assert!(rendered.contains("\x1b[0m"));
    }

    #[test]
    fn json_errors_are_machine_readable_and_quiet_only_affects_messages() {
        let error = WorkstateError::new(ErrorCategory::Cli, "invalid command")
            .with_context("argument", "environment");
        let policy = OutputPolicy {
            format: OutputFormat::Json,
            quiet: true,
            color: false,
        };
        let mut output = BufferOutput::default();
        assert!(policy.write_error(&mut output, &error).is_ok());
        assert!(output.stderr.contains("\"category\": \"CLI error\""));
        assert!(policy.write_message(&mut output, "hidden").is_ok());
        assert!(!output.stdout.contains("hidden"));
    }

    #[test]
    fn buffer_output_is_recordable_without_touching_the_terminal() {
        let mut output = BufferOutput::default();
        assert!(output.write_stdout("hello").is_ok());
        assert!(output.write_stderr("problem").is_ok());
        assert_eq!(output.stdout, "hello");
        assert_eq!(output.stderr, "problem");
    }
}
