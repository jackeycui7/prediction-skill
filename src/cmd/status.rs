/// status — show current agent state.

use anyhow::Result;
use serde_json::json;

use crate::client::ApiClient;
use crate::output::{Internal, Output};

/// Round a decimal chip string to 2 decimal places for display.
fn format_chips(s: &str) -> String {
    s.parse::<f64>()
        .map(|n| format!("{:.2}", n))
        .unwrap_or_else(|_| s.to_string())
}

pub fn run(server_url: &str) -> Result<()> {
    let client = ApiClient::new(server_url.to_string())?;

    let resp = match client.get_auth("/api/v1/agents/me/status") {
        Ok(v) => v,
        Err(e) => {
            Output::error(
                format!("Failed to fetch status: {e}"),
                "STATUS_FAILED",
                "network",
                true,
                "Check coordinator connectivity.",
                Internal {
                    next_action: "retry".into(),
                    next_command: Some("predict-agent status".into()),
                    ..Default::default()
                },
            )
            .print();
            return Ok(());
        }
    };

    let data = resp.get("data").cloned().unwrap_or(json!({}));

    let submissions = data
        .get("valid_submissions_today")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let balance_raw = data
        .get("balance")
        .and_then(|v| v.as_str())
        .unwrap_or("0");
    let balance = format_chips(balance_raw);
    let persona = data
        .get("persona")
        .and_then(|v| v.as_str())
        .unwrap_or("none");

    Output::success(
        format!(
            "Agent status: {} submissions today, {} chips balance, persona: {}.",
            submissions, balance, persona
        ),
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
