/// preflight — check all prerequisites before the main loop.
///
/// Checks (in order):
///   1. awp-wallet installed (or AWP_ADDRESS / AWP_PRIVATE_KEY set)
///   2. Coordinator reachable
///   3. Agent registered (has balance row / any activity)

use anyhow::Result;
use serde_json::json;

use crate::auth::get_address;
use crate::client::{check_server, ApiClient};
use crate::output::{Internal, Output};

pub fn run(server_url: &str) -> Result<()> {
    // Step 1: resolve wallet address
    let address = match get_address() {
        Ok(a) => a,
        Err(e) => {
            Output::error(
                format!("Cannot determine wallet address: {e}"),
                "WALLET_NOT_CONFIGURED",
                "dependency",
                false,
                "Set AWP_ADDRESS or AWP_PRIVATE_KEY, or install awp-wallet and run: awp-wallet unlock --duration 86400",
                Internal {
                    next_action: "configure_wallet".into(),
                    next_command: Some("awp-wallet unlock --duration 86400 --scope full".into()),
                    ..Default::default()
                },
            )
            .print();
            return Ok(());
        }
    };

    // Step 2: coordinator reachable
    if let Err(e) = check_server(server_url) {
        Output::error(
            format!("Cannot reach coordinator at {server_url}: {e}"),
            "COORDINATOR_UNREACHABLE",
            "network",
            true,
            format!("Check PREDICT_SERVER_URL and network. Tried: {server_url}"),
            Internal {
                next_action: "retry".into(),
                wait_seconds: Some(30),
                next_command: Some("predict-agent preflight".into()),
                ..Default::default()
            },
        )
        .print();
        return Ok(());
    }

    // Step 3: fetch agent status (this also registers the agent implicitly on first prediction)
    let client = ApiClient::new(server_url.to_string())?;
    let status = match client.get_auth("/api/v1/agents/me/status") {
        Ok(v) => v,
        Err(e) => {
            // Auth failed or network error
            Output::error(
                format!("Failed to fetch agent status: {e}"),
                "AUTH_FAILED",
                "auth",
                false,
                "Check your wallet configuration and ensure the timestamp is fresh.",
                Internal {
                    next_action: "configure_wallet".into(),
                    next_command: Some("predict-agent preflight".into()),
                    ..Default::default()
                },
            )
            .print();
            return Ok(());
        }
    };

    let data = status.get("data").cloned().unwrap_or(json!({}));
    let submissions_today = data.get("valid_submissions_today")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let balance_raw = data.get("balance")
        .and_then(|v| v.as_str())
        .unwrap_or("0");
    let balance = balance_raw.parse::<f64>()
        .map(|n| format!("{:.2}", n))
        .unwrap_or_else(|_| balance_raw.to_string());

    // Fetch open market count
    let open_markets = match client.get("/api/v1/markets/active") {
        Ok(v) => v.get("data")
            .and_then(|d| d.as_array())
            .map(|a| a.len())
            .unwrap_or(0),
        Err(_) => 0,
    };

    Output::success(
        format!(
            "Ready. {} open markets. {} submissions today. Balance: {} chips.",
            open_markets, submissions_today, balance
        ),
        json!({
            "status": "ready",
            "address": address,
            "open_markets": open_markets,
            "submissions_today": submissions_today,
            "balance": balance,
        }),
        Internal {
            next_action: "fetch_context".into(),
            next_command: Some("predict-agent context".into()),
            ..Default::default()
        },
    )
    .print();

    Ok(())
}
