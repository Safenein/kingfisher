//! Runtime-selectable log output formats.
//!
//! Every format shares the same `-v`/`-q` target filter and writes to stderr;
//! only the event formatter changes:
//!
//! - `text`: the historical human-readable output.
//! - `json`: one JSON object per line from `tracing-subscriber`'s JSON formatter.
//! - `template`: a small `{placeholder}` pattern, see [`TemplateFormat`].

mod fields;
mod template;

use std::io::IsTerminal;

use anyhow::{Result, bail};
use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};
use strum::Display;
use tracing_core::metadata::LevelFilter;
use tracing_subscriber::{
    Layer, Registry, filter::Targets, fmt, layer::SubscriberExt, util::SubscriberInitExt,
};

use crate::cli::config::LoggingConfig;
pub use template::TemplateFormat;

/// Output format for diagnostic logs written to stderr.
#[derive(
    Copy, Clone, Debug, Display, PartialEq, Eq, ValueEnum, Serialize, Deserialize, Default,
)]
#[strum(serialize_all = "kebab-case")]
#[serde(rename_all = "kebab-case")]
pub enum LogFormat {
    /// Human-readable text (default)
    #[default]
    Text,
    /// One JSON object per line
    Json,
    /// Custom pattern given by `--log-template`
    Template,
}

/// Global CLI flags controlling log output.
///
/// Non-exhaustive so new logging options do not break code that builds
/// [`crate::cli::global::GlobalArgs`]; start from `LogArgs::default()`.
#[derive(Args, Clone, Debug, Default)]
#[non_exhaustive]
pub struct LogArgs {
    /// Format of diagnostic logs written to stderr.
    ///
    /// - text: human-readable lines (default)
    /// - json: one JSON object per line
    /// - template: custom pattern from --log-template
    #[arg(
        id = "log_format",
        global = true,
        long = "log-format",
        value_enum,
        value_name = "FORMAT"
    )]
    pub format: Option<LogFormat>,

    /// Custom log line pattern, e.g. "{time} {level} {target}: {message} {fields}".
    ///
    /// Placeholders: {time}, {level}, {target}, {message}, {fields}, {field:NAME},
    /// {span}, {file}, {line}. Use {{ and }} for literal braces. Implies
    /// --log-format=template.
    #[arg(id = "log_template", global = true, long = "log-template", value_name = "PATTERN")]
    pub template: Option<String>,
}

/// Resolved logging configuration.
#[derive(Clone, Debug)]
pub struct LogSettings {
    /// Maximum level for Kingfisher's own targets.
    pub level: LevelFilter,
    /// Apply `level` to every target instead of limiting dependencies to errors.
    pub all_targets: bool,
    /// Explicit format; when absent, a template implies `template`, else `text`.
    pub format: Option<LogFormat>,
    /// Pattern used by the `template` format.
    pub template: Option<String>,
}

impl LogSettings {
    /// Resolve the effective format, rejecting contradictory combinations.
    pub fn effective_format(&self) -> Result<LogFormat> {
        match (self.format, &self.template) {
            (None, None) => Ok(LogFormat::Text),
            (None, Some(_)) => Ok(LogFormat::Template),
            (Some(LogFormat::Template), None) => {
                bail!(
                    "log format `template` requires a pattern (--log-template or logging.template)"
                )
            }
            (Some(LogFormat::Template), Some(_)) => Ok(LogFormat::Template),
            (Some(format), None) => Ok(format),
            (Some(format), Some(_)) => {
                bail!("a log template can only be used with log format `template`, not `{format}`")
            }
        }
    }

    fn filter(&self) -> Targets {
        if self.all_targets {
            // Enable the selected level for all modules
            Targets::new().with_default(self.level)
        } else {
            // Limit non-Kingfisher targets to errors at normal verbosity.
            Targets::new()
                .with_default(LevelFilter::ERROR) // Default for all modules
                .with_target("kingfisher", self.level)
        }
    }

    /// Build the formatting layer for `format`.
    fn layer<S>(&self, format: LogFormat) -> Result<Box<dyn Layer<S> + Send + Sync + 'static>>
    where
        S: tracing_core::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    {
        let ansi = std::io::stderr().is_terminal();
        let layer = match format {
            LogFormat::Text => fmt::layer()
                .with_writer(std::io::stderr)
                .with_target(true) // Include the tracing target in each log line
                .with_ansi(ansi) // Emit ANSI colours when stderr is a TTY
                .without_time()
                .boxed(),
            LogFormat::Json => fmt::layer()
                .json()
                .flatten_event(true)
                .with_current_span(true)
                .with_span_list(false)
                .with_writer(std::io::stderr)
                .boxed(),
            LogFormat::Template => {
                let pattern = self.template.as_deref().unwrap_or_default();
                fmt::layer()
                    .event_format(TemplateFormat::parse(pattern)?)
                    .with_ansi(ansi)
                    .with_writer(std::io::stderr)
                    .boxed()
            }
        };
        Ok(layer)
    }
}

/// Merge CLI and config choices. Format and template travel together: any CLI
/// choice replaces the config's pair, so `--log-format json` is never combined
/// with a configured template. The one exception is `--log-format template`
/// alone, which reuses the configured template.
pub fn resolve_choice(
    cli_format: Option<LogFormat>,
    cli_template: Option<String>,
    config: Option<&LoggingConfig>,
) -> (Option<LogFormat>, Option<String>) {
    match (config, cli_format, cli_template) {
        (Some(cfg), None, None) => (cfg.format, cfg.template.clone()),
        (Some(cfg), Some(LogFormat::Template), None) => {
            (Some(LogFormat::Template), cfg.template.clone())
        }
        (_, format, template) => (format, template),
    }
}

/// Install the global subscriber for `settings`.
pub fn init(settings: &LogSettings) -> Result<()> {
    let format = settings.effective_format()?;
    let layer = settings.layer::<Registry>(format)?;
    tracing_subscriber::registry().with(layer).with(settings.filter()).init();
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::fmt::MakeWriter;

    /// In-memory writer used to capture formatter output in tests.
    #[derive(Clone, Default)]
    pub struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Capture {
        pub fn contents(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Capture {
        type Writer = Capture;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(format: Option<LogFormat>, template: Option<&str>) -> LogSettings {
        LogSettings {
            level: LevelFilter::INFO,
            all_targets: false,
            format,
            template: template.map(str::to_string),
        }
    }

    #[test]
    fn effective_format_resolution() {
        let cases = [
            (None, None, Some(LogFormat::Text)),
            (None, Some("{message}"), Some(LogFormat::Template)),
            (Some(LogFormat::Json), None, Some(LogFormat::Json)),
            (Some(LogFormat::Template), Some("{message}"), Some(LogFormat::Template)),
            (Some(LogFormat::Template), None, None),
            (Some(LogFormat::Json), Some("{message}"), None),
        ];
        for (format, template, expected) in cases {
            let got = settings(format, template).effective_format().ok();
            assert_eq!(got, expected, "format={format:?} template={template:?}");
        }
    }

    #[test]
    fn invalid_template_is_reported_when_building_layer() {
        let err =
            settings(None, Some("{nope}")).layer::<Registry>(LogFormat::Template).err().unwrap();
        assert!(format!("{err:#}").contains("nope"));
    }

    #[test]
    fn logging_section_parses_and_rejects_unknown_keys() {
        let cfg = crate::cli::config::parse_str("logging:\n  format: json\n").unwrap();
        assert_eq!(cfg.logging.format, Some(LogFormat::Json));
        let cfg = crate::cli::config::parse_str("logging:\n  template: '{message}'\n").unwrap();
        assert_eq!(cfg.logging.template.as_deref(), Some("{message}"));
        assert!(crate::cli::config::parse_str("logging:\n  colour: red\n").is_err());
        assert!(crate::cli::config::parse_str("logging:\n  format: xml\n").is_err());
    }

    #[test]
    fn cli_choice_replaces_config_pair() {
        let cfg = LoggingConfig { format: Some(LogFormat::Template), template: Some("{x}".into()) };
        assert_eq!(
            resolve_choice(None, None, Some(&cfg)),
            (Some(LogFormat::Template), Some("{x}".into()))
        );
        assert_eq!(
            resolve_choice(Some(LogFormat::Json), None, Some(&cfg)),
            (Some(LogFormat::Json), None)
        );
        assert_eq!(
            resolve_choice(None, Some("{message}".into()), Some(&cfg)),
            (None, Some("{message}".into()))
        );
        assert_eq!(
            resolve_choice(Some(LogFormat::Json), None, None),
            (Some(LogFormat::Json), None)
        );
    }

    #[test]
    fn cli_template_format_reuses_config_pattern() {
        let cfg = LoggingConfig { format: Some(LogFormat::Json), template: Some("{x}".into()) };
        assert_eq!(
            resolve_choice(Some(LogFormat::Template), None, Some(&cfg)),
            (Some(LogFormat::Template), Some("{x}".into()))
        );
        assert_eq!(
            resolve_choice(Some(LogFormat::Template), None, None),
            (Some(LogFormat::Template), None)
        );
    }
}
