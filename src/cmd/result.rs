/// result -- query the outcome of a specific market and this agent's prediction.

use anyhow::Result;
use serde_json::json;

use crate::client::ApiClient;
use crate::output::{Internal, Output};

pub fn run(server_url: &str, market_id: &str) -> Result<()> {
    let client = ApiClient::new(server_url.to_string())?;

    let market_resp = match client.get(&format!("/api/v1/markets/{}", market_id)) {
        Ok(v) => v,
        Err(e) => {
            Output::error(
                format!("Market {} not found: {}", market_id, e),
                "MARKET_NOT_FOUND",
                "validation",
                false,
                format!("Check market ID. Use: predict-agent history"),
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

    let market = market_resp.get("data").cloned().unwrap_or(json!({}));
    let status = market.get("status").and_then(|v| v.as_str()).unwrap_or("");

    if status != "resolved" {
        let closes_at = market
            .get("close_at")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        Output::success(
            format!(
                "Market {} is still {}. Check back after it resolves (closes at {}).",
                market_id, status, closes_at
            ),
            json!({
                "market_id": market_id,
                "status": status,
                "close_at": closes_at,
                "outcome": null,
            }),
            Internal {
                next_action: "wait".into(),
                next_command: Some(format!("predict-agent result --market {}", market_id)),
                wait_seconds: Some(60),
                ..Default::default()
            },
        )
        .print();
        return Ok(());
    }

    let outcome = market.get("outcome").and_then(|v| v.as_str()).unwrap_or("?");
    let open_price = market
        .get("open_price")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let resolve_price = market
        .get("resolve_price")
        .and_then(|v| v.as_str())
        .unwrap_or("?");

    // Fetch my prediction for this market
    let my_preds = client
        .get_auth(&format!(
            "/api/v1/predictions/me?limit=500"
        ))
        .ok()
        .and_then(|v| v.get("data").and_then(|d| d.as_array()).cloned())
        .unwrap_or_default();

    let my_pred = my_preds
        .iter()
        .find(|p| p.get("market_id").and_then(|m| m.as_str()) == Some(market_id));

    let (user_msg, result_data) = match my_pred {
        Some(pred) => {
            let direction = pred.get("direction").and_then(|v| v.as_str()).unwrap_or("?");
            let correct = direction == outcome;
            let payout = pred
                .get("payout_chips")
                .and_then(|v| v.as_str())
                .unwrap_or("0");
            let filled = pred
                .get("tickets_filled")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);

            let msg = if correct {
                format!(
                    "Market {} resolved {}. You predicted {} — CORRECT! Payout: {} chips ({} tickets filled).",
                    market_id,
                    outcome.to_uppercase(),
                    direction.to_uppercase(),
                    payout,
                    filled
                )
            } else {
                format!(
                    "Market {} resolved {}. You predicted {} — WRONG. No payout ({} tickets filled).",
                    market_id,
                    outcome.to_uppercase(),
                    direction.to_uppercase(),
                    filled
                )
            };

            (
                msg,
                json!({
                    "market_id": market_id,
                    "outcome": outcome,
                    "open_price": open_price,
                    "resolve_price": resolve_price,
                    "your_prediction": direction,
                    "correct": correct,
                    "tickets_filled": filled,
                    "payout_received": payout,
                }),
            )
        }
        None => (
            format!(
                "Market {} resolved {}. You did not submit a prediction for this market.",
                market_id,
                outcome.to_uppercase()
            ),
            json!({
                "market_id": market_id,
                "outcome": outcome,
                "open_price": open_price,
                "resolve_price": resolve_price,
                "your_prediction": null,
                "correct": null,
            }),
        ),
    };

    Output::success(
        user_msg,
        result_data,
        Internal {
            next_action: "fetch_context".into(),
            next_command: Some("predict-agent context".into()),
            ..Default::default()
        },
    )
    .print();

    Ok(())
}
