//! Pattern-based text format, in the spirit of log4rs/logback patterns.
//!
//! Placeholders:
//!
//! | Placeholder     | Value                                              |
//! |-----------------|----------------------------------------------------|
//! | `{time}`        | RFC 3339 UTC timestamp                             |
//! | `{level}`       | `TRACE`..`ERROR`, coloured when ANSI is enabled    |
//! | `{target}`      | tracing target (module path by default)            |
//! | `{message}`     | event message                                      |
//! | `{fields}`      | remaining fields as space-separated `key=value`    |
//! | `{field:NAME}`  | a single field's value, empty when absent          |
//! | `{span}`        | active span names, `:`-separated from the root     |
//! | `{file}`        | source file, `/`-separated on every platform       |
//! | `{line}`        | source line                                        |
//!
//! `{{` and `}}` produce literal braces. A newline is appended to each event.

use std::fmt;

use anyhow::{Result, bail};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::{
    fmt::{FmtContext, FormatEvent, FormatFields, format::Writer},
    registry::LookupSpan,
};

use super::fields::{EventFields, display_value, source_file};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Literal(String),
    Time,
    Level,
    Target,
    Message,
    Fields,
    Field(String),
    Span,
    File,
    Line,
}

/// A parsed log pattern; see the module documentation for its syntax.
#[derive(Clone, Debug)]
pub struct TemplateFormat {
    tokens: Vec<Token>,
}

impl TemplateFormat {
    /// Parse `pattern`, rejecting unknown placeholders and unbalanced braces.
    pub fn parse(pattern: &str) -> Result<Self> {
        let mut tokens = Vec::new();
        let mut literal = String::new();
        let mut chars = pattern.char_indices().peekable();
        while let Some((start, c)) = chars.next() {
            match c {
                '{' if chars.next_if(|&(_, c)| c == '{').is_some() => literal.push('{'),
                '}' if chars.next_if(|&(_, c)| c == '}').is_some() => literal.push('}'),
                '{' => {
                    let Some(end) = pattern[start..].find('}').map(|i| start + i) else {
                        bail!("log template has an unclosed `{{` at byte {start}: {pattern}");
                    };
                    let name = &pattern[start + 1..end];
                    while chars.next_if(|&(i, _)| i <= end).is_some() {}
                    if !literal.is_empty() {
                        tokens.push(Token::Literal(std::mem::take(&mut literal)));
                    }
                    tokens.push(Self::placeholder(name)?);
                }
                '}' => bail!(
                    "log template has an unmatched `}}` at byte {start} (use `}}}}` for a literal brace): {pattern}"
                ),
                c => literal.push(c),
            }
        }
        if !literal.is_empty() {
            tokens.push(Token::Literal(literal));
        }
        Ok(Self { tokens })
    }

    fn placeholder(name: &str) -> Result<Token> {
        Ok(match name {
            "time" => Token::Time,
            "level" => Token::Level,
            "target" => Token::Target,
            "message" => Token::Message,
            "fields" => Token::Fields,
            "span" => Token::Span,
            "file" => Token::File,
            "line" => Token::Line,
            _ => match name.strip_prefix("field:") {
                Some(field) if !field.is_empty() => Token::Field(field.to_string()),
                _ => bail!(
                    "unknown log template placeholder `{{{name}}}`; expected one of {{time}}, \
                     {{level}}, {{target}}, {{message}}, {{fields}}, {{field:NAME}}, {{span}}, \
                     {{file}}, {{line}}"
                ),
            },
        })
    }
}

fn level_color(level: &Level) -> &'static str {
    match *level {
        Level::TRACE => "\x1b[35m",
        Level::DEBUG => "\x1b[34m",
        Level::INFO => "\x1b[32m",
        Level::WARN => "\x1b[33m",
        Level::ERROR => "\x1b[31m",
    }
}

impl<S, N> FormatEvent<S, N> for TemplateFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let meta = event.metadata();
        let EventFields { message, fields } = EventFields::collect(event);
        for token in &self.tokens {
            match token {
                Token::Literal(s) => writer.write_str(s)?,
                Token::Time => write!(
                    writer,
                    "{}",
                    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
                )?,
                Token::Level if writer.has_ansi_escapes() => {
                    write!(writer, "{}{}\x1b[0m", level_color(meta.level()), meta.level())?
                }
                Token::Level => write!(writer, "{}", meta.level())?,
                Token::Target => writer.write_str(meta.target())?,
                Token::Message => writer.write_str(message.as_deref().unwrap_or_default())?,
                Token::Fields => {
                    for (i, (key, value)) in fields.iter().enumerate() {
                        let sep = if i == 0 { "" } else { " " };
                        write!(writer, "{sep}{key}={}", display_value(value))?;
                    }
                }
                Token::Field(name) => {
                    if let Some(value) = fields.get(name) {
                        writer.write_str(&display_value(value))?;
                    }
                }
                Token::Span => {
                    if let Some(scope) = ctx.event_scope() {
                        for (i, span) in scope.from_root().enumerate() {
                            let sep = if i == 0 { "" } else { ":" };
                            write!(writer, "{sep}{}", span.name())?;
                        }
                    }
                }
                Token::File => writer.write_str(&source_file(meta).unwrap_or_default())?,
                Token::Line => {
                    if let Some(line) = meta.line() {
                        write!(writer, "{line}")?;
                    }
                }
            }
        }
        writeln!(writer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::test_support::Capture;

    fn render(pattern: &str, ansi: bool, f: impl FnOnce()) -> String {
        let out = Capture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(ansi)
            .event_format(TemplateFormat::parse(pattern).unwrap())
            .with_writer(out.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        out.contents()
    }

    #[test]
    fn renders_placeholders() {
        let out = render(
            "{level} {target}: {message} [{fields}] {field:rule}|{field:none}|",
            false,
            || {
                tracing::info!(target: "kf", rule = "aws", count = 2, "hello");
            },
        );
        // Field order follows serde_json's map, which depends on its `preserve_order` feature.
        assert!(
            out == "INFO kf: hello [rule=aws count=2] aws||\n"
                || out == "INFO kf: hello [count=2 rule=aws] aws||\n",
            "{out:?}"
        );
    }

    #[test]
    fn renders_source_file_with_forward_slashes() {
        let out = render("{file}", false, || tracing::info!("x"));
        assert!(out.trim_end().ends_with("logging/template.rs"), "{out:?}");
    }

    #[test]
    fn renders_spans_and_escaped_braces() {
        let out = render("{{{span}}} {message}}}", false, || {
            let _outer = tracing::info_span!("scan").entered();
            let _inner = tracing::info_span!("validate").entered();
            tracing::info!("x");
        });
        assert_eq!(out, "{scan:validate} x}\n");
    }

    #[test]
    fn colours_level_only_with_ansi() {
        let out = render("{level}", true, || tracing::warn!("x"));
        assert_eq!(out, "\x1b[33mWARN\x1b[0m\n");
    }

    #[test]
    fn rejects_malformed_patterns() {
        for (pattern, needle) in [
            ("{nope}", "unknown log template placeholder `{nope}`"),
            ("{field:}", "unknown log template placeholder"),
            ("{message", "unclosed"),
            ("message}", "unmatched"),
        ] {
            let err = TemplateFormat::parse(pattern).unwrap_err().to_string();
            assert!(err.contains(needle), "{pattern}: {err}");
        }
    }

    #[test]
    fn parses_into_tokens() {
        let parsed = TemplateFormat::parse("[{time}] {file}:{line}").unwrap();
        assert_eq!(
            parsed.tokens,
            [
                Token::Literal("[".into()),
                Token::Time,
                Token::Literal("] ".into()),
                Token::File,
                Token::Literal(":".into()),
                Token::Line,
            ]
        );
    }
}
