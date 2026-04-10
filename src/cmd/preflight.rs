/// preflight — check all prerequisites before the main loop.
///
/// Checks (in order):
///   1. awp-wallet installed (or AWP_ADDRESS / AWP_PRIVATE_KEY set)
///   2. AWP network registration (auto-register if needed, gasless)
///   3. Coordinator reachable
///   4. Agent status fetchable (auth works)

use anyhow::Result;
use serde_json::json;

use crate::auth::get_address;
use crate::awp_register;
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

    // Step 2: AWP network registration
    // Skip in dev mode (no real wallet to sign EIP-712 with)
    let is_dev = std::env::var("AWP_DEV_MODE").as_deref() == Ok("true")
        || std::env::var("AWP_DEV_MODE").as_deref() == Ok("1");

    if !is_dev {
        match awp_register::check_registration(&address) {
            Ok(true) => { /* already registered, continue */ }
            Ok(false) => {
                // Try auto-register
                let token = std::env::var("AWP_WALLET_TOKEN").unwrap_or_default();
                if token.is_empty() {
                    Output::error(
                        "Not registered on AWP network. Wallet token needed for auto-registration.",
                        "AWP_NOT_REGISTERED",
                        "dependency",
                        false,
                        "Run: awp-wallet unlock --duration 86400 --scope full, then set AWP_WALLET_TOKEN",
                        Internal {
                            next_action: "configure_wallet".into(),
                            next_command: Some("awp-wallet unlock --duration 86400 --scope full".into()),
                            ..Default::default()
                        },
                    )
                    .print();
                    return Ok(());
                }

                match awp_register::ensure_registered(&address, &token) {
                    Ok(result) if result.registered => {
                        // Registered (either was already or just auto-registered)
                        // Continue to next checks
                    }
                    Ok(result) => {
                        Output::error(
                            format!("AWP registration incomplete: {}", result.message),
                            "AWP_REGISTRATION_PENDING",
                            "dependency",
                            true,
                            "Registration submitted. Wait a moment and retry.",
                            Internal {
                                next_action: "retry".into(),
                                wait_seconds: Some(10),
                                next_command: Some("predict-agent preflight".into()),
                                ..Default::default()
                            },
                        )
                        .print();
                        return Ok(());
                    }
                    Err(e) => {
                        Output::error(
                            format!("AWP registration failed: {e}"),
                            "AWP_REGISTRATION_FAILED",
                            "dependency",
                            true,
                            "Check network connectivity to api.awp.sh and retry.",
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
                }
            }
            Err(_) => {
                // AWP API unreachable — don't block, just warn
                // The coordinator might still work without AWP registration in some setups
            }
        }
    }

    // Step 3: coordinator reachable
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

    // Step 4: fetch agent status (auth verification)
    let client = ApiClient::new(server_url.to_string())?;
    let status = match client.get_auth("/api/v1/agents/me/status") {
        Ok(v) => v,
        Err(e) => {
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
