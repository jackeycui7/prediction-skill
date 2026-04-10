/// Unified JSON output structs for all predict-agent commands.
///
/// Every command outputs a single JSON object with:
///   - ok: bool
///   - user_message: human-readable summary
///   - data: command-specific payload (null on error)
///   - error: error details (null on success)
///   - _internal: LLM-facing action hints

use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
pub struct Output {
    pub ok: bool,
    pub user_message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorDetail>,
    pub _internal: Internal,
}

#[derive(Serialize)]
pub struct ErrorDetail {
    pub code: String,
    pub category: String,
    pub retryable: bool,
    pub suggestion: String,
}

#[derive(Serialize, Default)]
pub struct Internal {
    pub next_action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submittable_markets: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<Value>,
}

impl Output {
    pub fn success(user_message: impl Into<String>, data: Value, internal: Internal) -> Self {
        Self {
            ok: true,
            user_message: user_message.into(),
            data: Some(data),
            error: None,
            _internal: internal,
        }
    }

    pub fn error(
        user_message: impl Into<String>,
        code: impl Into<String>,
        category: impl Into<String>,
        retryable: bool,
        suggestion: impl Into<String>,
        internal: Internal,
    ) -> Self {
        Self {
            ok: false,
            user_message: user_message.into(),
            data: None,
            error: Some(ErrorDetail {
                code: code.into(),
                category: category.into(),
                retryable,
                suggestion: suggestion.into(),
            }),
            _internal: internal,
        }
    }

    pub fn print(&self) {
        println!("{}", serde_json::to_string_pretty(self).unwrap());
    }
}
