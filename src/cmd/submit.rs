/// submit — submit a prediction to the coordinator.
///
/// Builds and signs the request, then POSTs to /api/v1/predictions.

use anyhow::Result;
use serde_json::json;

use crate::client::ApiClient;
use crate::output::{Internal, Output};

pub struct SubmitArgs {
    pub market: String,
    pub prediction: String,
    pub tickets: u32,
    pub reasoning: String,
    pub limit_price: Option<f64>,
    pub dry_run: bool,
}

pub fn run(server_url: &str, args: SubmitArgs) -> Result<()> {
    // Validate direction
    if args.prediction != "up" && args.prediction != "down" {
        Output::error(
            "Prediction must be 'up' or 'down'.",
            "INVALID_DIRECTION",
            "validation",
            false,
            "Use --prediction up or --prediction down.",
            Internal {
                next_action: "fix_command".into(),
                ..Default::default()
            },
        )
        .print();
        return Ok(());
    }

    if args.tickets == 0 {
        Output::error(
            "Tickets must be greater than 0.",
            "INVALID_TICKETS",
            "validation",
            false,
            "Use --tickets N where N >= 1.",
            Internal {
                next_action: "fix_command".into(),
                ..Default::default()
            },
        )
        .print();
        return Ok(());
    }

    if let Some(lp) = args.limit_price {
        if !(0.01..=0.99).contains(&lp) {
            Output::error(
                format!("limit-price must be between 0.01 and 0.99, got {lp}"),
                "INVALID_LIMIT_PRICE",
                "validation",
                false,
                "Use --limit-price 0.01 to 0.99.",
                Internal {
                    next_action: "fix_command".into(),
                    ..Default::default()
                },
            )
            .print();
            return Ok(());
        }
    }

    let mut body = json!({
        "market_id": args.market,
        "prediction": args.prediction,
        "tickets": args.tickets,
        "reasoning": args.reasoning,
    });

    if let Some(lp) = args.limit_price {
        body["limit_price"] = json!(lp);
    }

    if args.dry_run {
        Output::success(
            format!(
                "[dry-run] Would submit {} prediction for market {} with {} tickets.",
                args.prediction.to_uppercase(),
                args.market,
                args.tickets
            ),
            json!({
                "dry_run": true,
                "would_submit": body,
            }),
            Internal {
                next_action: "submit".into(),
                next_command: Some(format!(
                    "predict-agent submit --market {} --prediction {} --tickets {} --reasoning \"{}\"{}",
                    args.market,
                    args.prediction,
                    args.tickets,
                    args.reasoning.chars().take(50).collect::<String>(),
                    if args.limit_price.is_some() {
                        format!(" --limit-price {}", args.limit_price.unwrap())
                    } else {
                        String::new()
                    }
                )),
                ..Default::default()
            },
        )
        .print();
        return Ok(());
    }

    let client = ApiClient::new(server_url.to_string())?;

    let resp = match client.post_auth("/api/v1/predictions", &body) {
        Ok(v) => v,
        Err(e) => {
            let err_str = e.to_string();
            // Parse error details from server response if present
            let (code, category, retryable, suggestion) =
                parse_server_error(&err_str);

            Output::error(
                format!("Submission failed: {}", extract_message(&err_str)),
                code,
                category,
                retryable,
                suggestion,
                Internal {
                    next_action: if retryable { "retry".into() } else { "fix_command".into() },
                    wait_seconds: if retryable { Some(30) } else { None },
                    next_command: Some("predict-agent context".into()),
                    ..Default::default()
                },
            )
            .print();
            return Ok(());
        }
    };

    let data = resp.get("data").cloned().unwrap_or(json!({}));

    // Extract key fields for user_message
    let direction = data.get("direction").and_then(|v| v.as_str()).unwrap_or(&args.prediction);
    let filled = data.get("tickets_filled").and_then(|v| v.as_i64()).unwrap_or(0);
    let total = args.tickets as i64;
    let status = data.get("order_status").and_then(|v| v.as_str()).unwrap_or("open");
    // payout_if_correct is an integer in the response
    let payout = data
        .get("payout_if_correct")
        .and_then(|v| v.as_i64())
        .map(|n| n.to_string())
        .unwrap_or_else(|| "0".to_string());

    let market_id_short = args.market.clone();

    let user_msg = match status {
        "filled" => format!(
            "Submitted {} for {}. Filled {}/{} tickets. Payout if correct: {} chips.",
            direction.to_uppercase(),
            market_id_short,
            filled,
            total,
            payout
        ),
        "partial" => format!(
            "Submitted {} for {}. Partially filled {}/{} tickets. Unfilled tickets auto-refund at close.",
            direction.to_uppercase(),
            market_id_short,
            filled,
            total,
        ),
        _ => format!(
            "Submitted {} for {}. {} tickets queued (no immediate fill). Chips locked until market close.",
            direction.to_uppercase(),
            market_id_short,
            total
        ),
    };

    Output::success(
        user_msg,
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

/// Extract a human-readable message from the error string.
fn extract_message(err: &str) -> String {
    // Try to parse as JSON first (server error response)
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(err) {
        if let Some(msg) = v.get("error").and_then(|e| e.get("message")).and_then(|m| m.as_str()) {
            return msg.to_string();
        }
        if let Some(msg) = v.get("message").and_then(|m| m.as_str()) {
            return msg.to_string();
        }
    }
    // Try to extract JSON from "HTTP 4xx: {json}" format
    if let Some(json_start) = err.find('{') {
        let json_part = &err[json_start..];
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_part) {
            if let Some(msg) = v.get("error").and_then(|e| e.get("message")).and_then(|m| m.as_str()) {
                return msg.to_string();
            }
        }
    }
    err.to_string()
}

/// Parse structured error code/category/retryable from server error response.
fn parse_server_error(err: &str) -> (String, String, bool, String) {
    let try_parse = |json_str: &str| -> Option<(String, String, bool, String)> {
        let v: serde_json::Value = serde_json::from_str(json_str).ok()?;
        let err_obj = v.get("error")?;
        let code = err_obj.get("code")?.as_str()?.to_string();
        let category = err_obj
            .get("category")
            .and_then(|c| c.as_str())
            .unwrap_or("unknown")
            .to_string();
        let retryable = err_obj
            .get("retryable")
            .and_then(|r| r.as_bool())
            .unwrap_or(false);
        let suggestion = err_obj
            .get("suggestion")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        Some((code, category, retryable, suggestion))
    };

    // Try raw err as JSON
    if let Some(result) = try_parse(err) {
        return result;
    }
    // Try to find JSON portion after "HTTP 4xx: "
    if let Some(json_start) = err.find('{') {
        if let Some(result) = try_parse(&err[json_start..]) {
            return result;
        }
    }

    // Defaults based on common patterns
    if err.contains("RATE_LIMIT") || err.contains("429") {
        return ("RATE_LIMIT_EXCEEDED".into(), "rate_limit".into(), true, "Wait and retry.".into());
    }
    if err.contains("MARKET_CLOSED") {
        return ("MARKET_CLOSED".into(), "validation".into(), false, "Choose an open market.".into());
    }

    ("SUBMISSION_FAILED".into(), "unknown".into(), false, "Check the error details and retry.".into())
}
