//! Field collection shared by the custom formatters.

use std::fmt::Debug;

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};

/// An event's `message` and its remaining fields as JSON values.
#[derive(Default)]
pub(super) struct EventFields {
    pub message: Option<String>,
    pub fields: Map<String, Value>,
}

impl EventFields {
    pub fn collect(event: &tracing::Event<'_>) -> Self {
        let mut fields = Self::default();
        event.record(&mut fields);
        fields
    }

    fn insert(&mut self, field: &Field, value: Value) {
        if field.name() == "message" {
            self.message = Some(match value {
                Value::String(s) => s,
                other => other.to_string(),
            });
        } else {
            self.fields.insert(field.name().to_string(), value);
        }
    }
}

impl Visit for EventFields {
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.insert(field, serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.insert(field, value.into());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.insert(field, value.into());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.insert(field, value.into());
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.insert(field, value.into());
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.insert(field, value.to_string().into());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        self.insert(field, format!("{value:?}").into());
    }
}

/// The event's source file with `/` separators, so output matches across platforms.
pub(super) fn source_file(meta: &tracing::Metadata<'_>) -> Option<String> {
    meta.file().map(|file| file.replace('\\', "/"))
}

/// Render a field value for plain-text output: strings without JSON quotes.
pub(super) fn display_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
