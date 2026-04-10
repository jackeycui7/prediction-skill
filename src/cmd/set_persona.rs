/// set-persona — update the agent's persona (7-day cooldown).

use anyhow::Result;
use serde_json::json;

use crate::client::ApiClient;
use crate::output::{Internal, Output};

pub const VALID_PERSONAS: &[&str] = &[
    "quant_trader",
    "macro_analyst",
    "crypto_native",
    "academic_economist",
    "geopolitical_analyst",
    "tech_industry",
    "on_chain_analyst",
    "retail_sentiment",
];

pub fn run(server_url: &str, persona: &str) -> Result<()> {
    if !VALID_PERSONAS.contains(&persona) {
        Output::error(
            format!("Invalid persona '{}'. Valid options: {}", persona, VALID_PERSONAS.join(", ")),
            "INVALID_PERSONA",
            "validation",
            false,
            format!("Use one of: {}", VALID_PERSONAS.join(", ")),
            Internal {
                next_action: "fix_command".into(),
                next_command: Some("predict-agent set-persona <persona>".into()),
                ..Default::default()
            },
        )
        .print();
        return Ok(());
    }

    let client = ApiClient::new(server_url.to_string())?;

    let body = json!({ "persona": persona });
    let resp = match client.post_auth("/api/v1/agents/me/persona", &body) {
        Ok(v) => v,
        Err(e) => {
            let err_str = e.to_string();
            let (retryable, suggestion) = if err_str.contains("PERSONA_COOLDOWN") || err_str.contains("cooldown") {
                (false, "Persona can only be changed once every 7 days.".to_string())
            } else {
                (true, "Check coordinator connectivity and retry.".to_string())
            };
            Output::error(
                format!("Failed to set persona: {}", extract_message(&err_str)),
                "SET_PERSONA_FAILED",
                if retryable { "network" } else { "validation" },
                retryable,
                suggestion,
                Internal {
                    next_action: "fetch_context".into(),
                    next_command: Some("predict-agent context".into()),
                    ..Default::default()
                },
            )
            .print();
            return Ok(());
        }
    };

    let data = resp.get("data").cloned().unwrap_or(json!({}));

    Output::success(
        format!("Persona updated to '{}'. 7-day cooldown started.", persona),
        data,
        Internal {
            next_action: "fetch_context".into(),
            next_command: Some("predict-agent context".into()),
            ..Default::default()
        },
    )
    .print();

    Ok(())
}

fn extract_message(err: &str) -> String {
    if let Some(json_start) = err.find('{') {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&err[json_start..]) {
            if let Some(msg) = v.get("error").and_then(|e| e.get("message")).and_then(|m| m.as_str()) {
                return msg.to_string();
            }
        }
    }
    err.to_string()
}
