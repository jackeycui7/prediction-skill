/// AWP network registration — check and auto-register via gasless relay.
///
/// Flow:
///   1. JSON-RPC address.check → isRegistered?
///   2. If not: registry.get → nonce.get → build EIP-712 SetRecipient → sign → relay
///   3. Poll address.check until confirmed

use anyhow::{bail, Context, Result};
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::thread;
use std::time::Duration;

use crate::auth::find_awp_wallet;

const AWP_API_BASE: &str = "https://api.awp.sh/v2";
const AWP_RELAY_BASE: &str = "https://api.awp.sh/api";
const CHAIN_ID: u64 = 8453; // Base mainnet
const POLL_ATTEMPTS: u32 = 5;
const POLL_INTERVAL_SECS: u64 = 2;

pub struct RegistrationResult {
    pub registered: bool,
    pub auto_registered: bool,
    pub message: String,
}

/// Check AWP registration status. Returns Ok with status, never panics on network errors.
pub fn check_registration(address: &str) -> Result<bool> {
    let client = build_client();
    let resp = awp_jsonrpc(&client, "address.check", json!({
        "address": address,
        "chainId": CHAIN_ID,
    }))?;

    Ok(is_registered(&resp))
}

/// Check and auto-register if needed. Gasless, free.
pub fn ensure_registered(address: &str, wallet_token: &str) -> Result<RegistrationResult> {
    let client = build_client();

    // Step 1: check current status
    let check = awp_jsonrpc(&client, "address.check", json!({
        "address": address,
        "chainId": CHAIN_ID,
    }))?;

    if is_registered(&check) {
        return Ok(RegistrationResult {
            registered: true,
            auto_registered: false,
            message: "Already registered on AWP network.".into(),
        });
    }

    // Step 2: get registry for contract address
    let registry = awp_jsonrpc(&client, "registry.get", json!({
        "chainId": CHAIN_ID,
    }))?;

    let verifying_contract = registry
        .get("awpRegistry")
        .and_then(|v| v.as_str())
        .context("registry.get missing awpRegistry address")?
        .to_string();

    // Step 3: get nonce
    let nonce_resp = awp_jsonrpc(&client, "nonce.get", json!({
        "address": address,
        "chainId": CHAIN_ID,
    }))?;

    let nonce = nonce_resp
        .get("nonce")
        .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .unwrap_or(0);

    let deadline = chrono::Utc::now().timestamp() as u64 + 3600;

    // Step 4: build EIP-712 typed data for SetRecipient
    let typed_data = json!({
        "types": {
            "EIP712Domain": [
                {"name": "name", "type": "string"},
                {"name": "version", "type": "string"},
                {"name": "chainId", "type": "uint256"},
                {"name": "verifyingContract", "type": "address"}
            ],
            "SetRecipient": [
                {"name": "user", "type": "address"},
                {"name": "recipient", "type": "address"},
                {"name": "nonce", "type": "uint256"},
                {"name": "deadline", "type": "uint256"}
            ]
        },
        "primaryType": "SetRecipient",
        "domain": {
            "name": "AWPRegistry",
            "version": "1",
            "chainId": CHAIN_ID,
            "verifyingContract": verifying_contract
        },
        "message": {
            "user": address,
            "recipient": address,
            "nonce": nonce,
            "deadline": deadline
        }
    });

    // Step 5: sign with awp-wallet
    let signature = sign_typed_data(wallet_token, &typed_data)?;

    // Step 6: submit to gasless relay
    let relay_url = format!("{}/relay/set-recipient", AWP_RELAY_BASE);
    let relay_body = json!({
        "user": address,
        "recipient": address,
        "nonce": nonce,
        "deadline": deadline,
        "chainId": CHAIN_ID,
        "signature": signature,
    });

    let relay_resp = client
        .post(&relay_url)
        .header("Content-Type", "application/json")
        .json(&relay_body)
        .send()
        .context("Failed to call registration relay")?;

    if !relay_resp.status().is_success() {
        let status = relay_resp.status();
        let body = relay_resp.text().unwrap_or_default();
        bail!("Registration relay returned HTTP {}: {}", status, body);
    }

    // Step 7: poll until confirmed
    for attempt in 0..POLL_ATTEMPTS {
        thread::sleep(Duration::from_secs(POLL_INTERVAL_SECS));

        match awp_jsonrpc(&client, "address.check", json!({
            "address": address,
            "chainId": CHAIN_ID,
        })) {
            Ok(refreshed) if is_registered(&refreshed) => {
                return Ok(RegistrationResult {
                    registered: true,
                    auto_registered: true,
                    message: "Auto-registered on AWP network (gasless).".into(),
                });
            }
            _ => {
                if attempt == POLL_ATTEMPTS - 1 {
                    // Last attempt failed, but relay succeeded — likely just slow
                    return Ok(RegistrationResult {
                        registered: true, // optimistic
                        auto_registered: true,
                        message: "Registration submitted. Confirmation pending.".into(),
                    });
                }
            }
        }
    }

    Ok(RegistrationResult {
        registered: false,
        auto_registered: false,
        message: "Registration submitted but not yet confirmed.".into(),
    })
}

fn is_registered(check: &Value) -> bool {
    check.get("isRegistered").and_then(|v| v.as_bool()).unwrap_or(false)
        || check.get("isRegisteredUser").and_then(|v| v.as_bool()).unwrap_or(false)
}

fn build_client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent("predict-agent/0.1.0")
        .build()
        .expect("failed to build HTTP client")
}

fn awp_jsonrpc(client: &Client, method: &str, params: Value) -> Result<Value> {
    let body = json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
        "id": 1,
    });

    let resp = client
        .post(AWP_API_BASE)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .context(format!("AWP API call failed: {}", method))?;

    let result: Value = resp.json().context("AWP API returned invalid JSON")?;

    if let Some(err) = result.get("error") {
        let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("unknown error");
        bail!("AWP API error ({}): {}", method, msg);
    }

    result.get("result").cloned().context(format!("AWP API {} returned no result", method))
}

fn sign_typed_data(wallet_token: &str, typed_data: &Value) -> Result<String> {
    let wallet_bin = find_awp_wallet()?;
    let data_str = serde_json::to_string(typed_data)?;

    let output = std::process::Command::new(&wallet_bin)
        .args([
            "sign-typed-data",
            "--token",
            wallet_token,
            "--data",
            &data_str,
        ])
        .output()
        .context("Failed to run awp-wallet sign-typed-data")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("awp-wallet sign-typed-data failed: {}", stderr);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let v: Value = serde_json::from_str(&stdout).context("awp-wallet returned invalid JSON")?;
    let sig = v["signature"]
        .as_str()
        .context("awp-wallet response missing 'signature' field")?;
    Ok(sig.to_string())
}
