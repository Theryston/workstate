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
            OutputFormat::Human => Ok(error.render()),
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
