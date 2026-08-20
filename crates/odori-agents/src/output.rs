//! Typed outputs: a run produces a value, not a string.
//!
//! The run-loop workflow always records the final turn text; typing happens
//! at the runner boundary via [`AgentOutput`]. `String` passes text
//! through; [`Json<T>`] parses it as JSON into any `Deserialize` type.
//! When the agent carries an output schema
//! ([`crate::agent::Agent::with_output_schema`]), providers that can
//! enforce it at the backend do (headless Claude Code: `--json-schema`),
//! making the parse a formality rather than a hope.

use serde::de::DeserializeOwned;
use serde_json::Value;
use thiserror::Error;

/// A type a run can produce.
pub trait AgentOutput: Sized + Send {
    /// JSON Schema for this output, when one exists. Passed to backends
    /// that can enforce output shape.
    fn schema() -> Option<Value> {
        None
    }

    /// Interpret the run's final text as this type.
    fn parse(text: &str) -> Result<Self, OutputParseError>;
}

impl AgentOutput for String {
    fn parse(text: &str) -> Result<Self, OutputParseError> {
        Ok(text.to_owned())
    }
}

/// Wrapper marking an output as JSON-typed: the final text is parsed with
/// `serde_json` into `T`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Json<T>(pub T);

impl<T: DeserializeOwned + Send> AgentOutput for Json<T> {
    fn parse(text: &str) -> Result<Self, OutputParseError> {
        serde_json::from_str(text)
            .map(Json)
            .map_err(|error| OutputParseError {
                message: error.to_string(),
                text: text.to_owned(),
            })
    }
}

/// The final text did not deserialize into the requested output type.
#[derive(Debug, Clone, Error)]
#[error("run output did not parse as the requested type: {message}")]
pub struct OutputParseError {
    /// The deserializer's complaint.
    pub message: String,
    /// The raw final text, for diagnosis.
    pub text: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[test]
    fn string_output_passes_text_through() {
        let out = String::parse("plain text").expect("string output");
        assert_eq!(out, "plain text");
    }

    #[test]
    fn json_output_parses_and_reports_failures() {
        #[derive(Debug, Deserialize, PartialEq)]
        struct Verdict {
            ok: bool,
        }
        let Json(verdict) = Json::<Verdict>::parse(r#"{"ok":true}"#).expect("valid json");
        assert!(verdict.ok);
        let error = Json::<Verdict>::parse("not json").expect_err("invalid json");
        assert_eq!(error.text, "not json");
    }
}
