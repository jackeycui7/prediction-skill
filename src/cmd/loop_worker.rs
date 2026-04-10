/// loop_worker — background prediction loop.
///
/// Runs continuously: fetch context → call LLM for analysis → submit prediction → sleep.
/// LLM is invoked via OpenClaw CLI (`openclaw agent --agent <id> --message <prompt>`),
/// matching the pattern used by benchmark-skill and mine-skill.
///
/// Usage: predict-agent loop [--interval 120] [--max-iterations 0] [--agent-id predict-worker]
///
/// The loop handles:
///   - Automatic context fetching each round
///   - LLM prompt construction with klines data
///   - Parsing LLM response (direction + reasoning)
///   - Submission with error recovery
///   - Adaptive backoff on empty markets or errors
///   - Graceful shutdown on SIGINT/SIGTERM

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::client::ApiClient;
use crate::{log_debug, log_error, log_info, log_warn};

pub struct LoopArgs {
    pub interval: u64,
    pub max_iterations: u64,
    pub agent_id: String,
}

pub fn run(server_url: &str, args: LoopArgs) -> Result<()> {
    log_info!(
        "loop: starting (interval={}s, max_iter={}, agent={}, server={})",
        args.interval,
        args.max_iterations,
        args.agent_id,
        server_url
    );

    // Set up graceful shutdown
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        eprintln!("\n[predict-agent] loop: received shutdown signal, finishing current round...");
        r.store(false, Ordering::SeqCst);
    })
    .ok(); // Ignore error if handler already set

    // Detect OpenClaw CLI
    let openclaw_bin = detect_openclaw();
    if openclaw_bin.is_none() {
        log_error!("loop: openclaw CLI not found. Install OpenClaw or add it to PATH.");
        log_error!("loop: the prediction loop requires an LLM to analyze markets and generate reasoning.");
        eprintln!("\npredict-agent loop requires the OpenClaw CLI (openclaw) to be installed.");
        eprintln!("The loop calls an LLM each round to analyze klines and write original reasoning.");
        eprintln!("\nInstall: https://docs.openclaw.com/install");
        return Ok(());
    }
    let openclaw_bin = openclaw_bin.unwrap();
    log_info!("loop: using openclaw at {}", openclaw_bin);

    // Ensure agent exists
    ensure_agent(&openclaw_bin, &args.agent_id);

    let mut iteration: u64 = 0;
    let mut consecutive_empty = 0u32;
    let mut consecutive_errors = 0u32;

    while running.load(Ordering::SeqCst) {
        iteration += 1;
        if args.max_iterations > 0 && iteration > args.max_iterations {
            log_info!("loop: reached max iterations ({}), stopping", args.max_iterations);
            break;
        }

        log_info!("loop: === iteration {} ===", iteration);
        let iter_start = Instant::now();

        match run_iteration(server_url, &openclaw_bin, &args.agent_id) {
            IterationResult::Submitted { market, direction } => {
                log_info!(
                    "loop: submitted {} for {} ({:.1}s)",
                    direction,
                    market,
                    iter_start.elapsed().as_secs_f64()
                );
                consecutive_empty = 0;
                consecutive_errors = 0;
            }
            IterationResult::NoMarkets { wait_seconds } => {
                consecutive_empty += 1;
                let backoff = calculate_backoff(args.interval, consecutive_empty, Some(wait_seconds));
                log_info!(
                    "loop: no submittable markets (consecutive={}), sleeping {}s",
                    consecutive_empty,
                    backoff
                );
                interruptible_sleep(backoff, &running);
                continue;
            }
            IterationResult::RateLimited { wait_seconds } => {
                log_info!("loop: rate limited, sleeping {}s", wait_seconds);
                interruptible_sleep(wait_seconds, &running);
                continue;
            }
            IterationResult::LlmFailed { reason } => {
                consecutive_errors += 1;
                let backoff = calculate_backoff(args.interval, consecutive_errors, None);
                log_warn!(
                    "loop: LLM call failed ({}), sleeping {}s (errors={})",
                    reason,
                    backoff,
                    consecutive_errors
                );
                interruptible_sleep(backoff, &running);
                continue;
            }
            IterationResult::Error { reason } => {
                consecutive_errors += 1;
                let backoff = calculate_backoff(args.interval, consecutive_errors, None);
                log_error!(
                    "loop: iteration error ({}), sleeping {}s (errors={})",
                    reason,
                    backoff,
                    consecutive_errors
                );
                interruptible_sleep(backoff, &running);
                continue;
            }
        }

        // Normal sleep between iterations
        log_debug!("loop: sleeping {}s until next iteration", args.interval);
        interruptible_sleep(args.interval, &running);
    }

    log_info!("loop: stopped after {} iterations", iteration);
    Ok(())
}

enum IterationResult {
    Submitted {
        market: String,
        direction: String,
    },
    NoMarkets {
        wait_seconds: u64,
    },
    RateLimited {
        wait_seconds: u64,
    },
    LlmFailed {
        reason: String,
    },
    Error {
        reason: String,
    },
}

fn run_iteration(server_url: &str, openclaw_bin: &str, agent_id: &str) -> IterationResult {
    // 1. Create API client
    let client = match ApiClient::new(server_url.to_string()) {
        Ok(c) => c,
        Err(e) => {
            return IterationResult::Error {
                reason: format!("API client init failed: {e}"),
            }
        }
    };

    // 2. Fetch agent status
    let status = match client.get_auth("/api/v1/agents/me/status") {
        Ok(v) => v,
        Err(e) => {
            return IterationResult::Error {
                reason: format!("status fetch failed: {e}"),
            }
        }
    };
    let agent_data = status.get("data").cloned().unwrap_or(json!({}));
    let balance = agent_data
        .get("balance")
        .and_then(|v| v.as_str().and_then(|s| s.parse::<f64>().ok()).or_else(|| v.as_f64()))
        .unwrap_or(0.0);
    let persona = agent_data
        .get("persona")
        .and_then(|v| v.as_str())
        .unwrap_or("none");
    log_debug!("loop: balance={:.0}, persona={}", balance, persona);

    // 3. Fetch active markets
    let markets_resp = match client.get("/api/v1/markets/active") {
        Ok(v) => v,
        Err(e) => {
            return IterationResult::Error {
                reason: format!("markets fetch failed: {e}"),
            }
        }
    };
    let markets = markets_resp
        .get("data")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // 4. Fetch my predictions to find submitted markets
    let my_preds: Vec<String> = client
        .get_auth("/api/v1/predictions/me?limit=200")
        .ok()
        .and_then(|v| v.get("data").and_then(|d| d.as_array()).cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|p| p.get("market_id").and_then(|m| m.as_str()).map(str::to_string))
        .collect();

    // 5. Find submittable markets (not submitted, >120s remaining)
    let now = chrono::Utc::now();
    let mut submittable: Vec<Value> = markets
        .iter()
        .filter_map(|m| {
            let id = m.get("id")?.as_str()?;
            let close_at_str = m.get("close_at")?.as_str()?;
            let close_at: chrono::DateTime<chrono::Utc> = close_at_str.parse().ok()?;
            let closes_in = (close_at - now).num_seconds();
            if closes_in < 120 || my_preds.contains(&id.to_string()) {
                return None;
            }
            let up = m.get("up_tickets_filled").and_then(|v| v.as_i64()).unwrap_or(0);
            let down = m.get("down_tickets_filled").and_then(|v| v.as_i64()).unwrap_or(0);
            let total = up + down;
            let implied_up = if total > 0 { up as f64 / total as f64 } else { 0.5 };
            Some(json!({
                "id": id,
                "asset": m.get("asset").and_then(|v| v.as_str()).unwrap_or(""),
                "window": m.get("window").and_then(|v| v.as_str()).unwrap_or(""),
                "closes_in_seconds": closes_in,
                "implied_up_prob": (implied_up * 100.0).round() / 100.0,
                "up_tickets": up,
                "down_tickets": down,
                "participant_count": m.get("participant_count").and_then(|v| v.as_i64()).unwrap_or(0),
            }))
        })
        .collect();

    if submittable.is_empty() {
        return IterationResult::NoMarkets { wait_seconds: 300 };
    }

    // Sort by closes_in ascending
    submittable.sort_by_key(|m| {
        m.get("closes_in_seconds")
            .and_then(|v| v.as_i64())
            .unwrap_or(i64::MAX)
    });

    log_info!("loop: {} submittable markets", submittable.len());

    // 6. Pick recommended market and fetch klines
    let recommended = &submittable[0];
    let market_id = recommended["id"].as_str().unwrap().to_string();

    let klines_data = client
        .get(&format!("/api/v1/markets/{}/klines", market_id))
        .ok()
        .and_then(|resp| {
            resp.get("data")
                .and_then(|d| d.get("klines"))
                .and_then(|k| k.as_array())
                .cloned()
        });

    let kline_count = klines_data.as_ref().map(|k| k.len()).unwrap_or(0);
    log_info!("loop: recommended={}, klines={} candles", market_id, kline_count);

    // 7. Build LLM prompt
    let prompt = build_prompt(
        &market_id,
        recommended,
        &klines_data,
        &submittable,
        balance,
        persona,
    );

    // 8. Call LLM via OpenClaw
    log_info!("loop: calling LLM via openclaw agent {}...", agent_id);
    let llm_start = Instant::now();
    let llm_response = call_openclaw(openclaw_bin, agent_id, &prompt);
    let llm_elapsed = llm_start.elapsed();

    let llm_text = match llm_response {
        Ok(text) => {
            log_info!("loop: LLM responded ({:.1}s, {} chars)", llm_elapsed.as_secs_f64(), text.len());
            log_debug!("loop: LLM raw output: {}", &text[..text.len().min(500)]);
            text
        }
        Err(e) => {
            return IterationResult::LlmFailed {
                reason: format!("{e}"),
            }
        }
    };

    // 9. Parse LLM response
    let (direction, reasoning, tickets, target_market) = match parse_llm_response(&llm_text) {
        Ok(parsed) => parsed,
        Err(e) => {
            log_warn!("loop: failed to parse LLM response: {}", e);
            return IterationResult::LlmFailed {
                reason: format!("parse failed: {e}"),
            };
        }
    };

    // Use target market from LLM if valid, otherwise use recommended
    let final_market = if let Some(ref tm) = target_market {
        if submittable.iter().any(|m| m["id"].as_str() == Some(tm.as_str())) {
            tm.clone()
        } else {
            log_warn!("loop: LLM suggested market {} not in submittable list, using {}", tm, market_id);
            market_id.clone()
        }
    } else {
        market_id.clone()
    };

    let final_tickets = tickets.unwrap_or_else(|| {
        // Default: ~5% of balance, minimum 1
        let t = (balance * 0.05).floor() as u32;
        t.max(1)
    });

    log_info!(
        "loop: submitting {} {} tickets for {}",
        direction,
        final_tickets,
        final_market
    );

    // 10. Submit prediction
    let body = json!({
        "market_id": final_market,
        "prediction": direction,
        "tickets": final_tickets,
        "reasoning": reasoning,
    });

    match client.post_auth("/api/v1/predictions", &body) {
        Ok(resp) => {
            let status = resp
                .get("data")
                .and_then(|d| d.get("order_status"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            log_info!("loop: submission accepted (order_status={})", status);
            IterationResult::Submitted {
                market: final_market,
                direction,
            }
        }
        Err(e) => {
            let err_str = e.to_string();
            if err_str.contains("RATE_LIMIT") || err_str.contains("429") {
                return IterationResult::RateLimited { wait_seconds: 300 };
            }
            if err_str.contains("INSUFFICIENT_BALANCE") {
                log_warn!("loop: insufficient balance, waiting for chip feed");
                return IterationResult::NoMarkets { wait_seconds: 600 };
            }
            IterationResult::Error {
                reason: format!("submit failed: {}", extract_short_error(&err_str)),
            }
        }
    }
}

fn build_prompt(
    market_id: &str,
    recommended: &Value,
    klines: &Option<Vec<Value>>,
    all_markets: &[Value],
    balance: f64,
    persona: &str,
) -> String {
    let asset = recommended["asset"].as_str().unwrap_or("BTC/USDT");
    let window = recommended["window"].as_str().unwrap_or("15m");
    let implied_up = recommended["implied_up_prob"].as_f64().unwrap_or(0.5);
    let closes_in = recommended["closes_in_seconds"].as_i64().unwrap_or(0);

    let mut prompt = String::with_capacity(6000);

    // Identity and context
    prompt.push_str(&format!(
        "You are a prediction agent competing in AWP Predict WorkNet{}.\n\n",
        if persona != "none" { format!(" (persona: {})", persona) } else { String::new() }
    ));

    // Game rules — the agent must understand the full picture
    prompt.push_str("## Game Rules\n\n");
    prompt.push_str("You are playing a prediction market game against other AI agents. Understanding the rules is critical to making profitable decisions.\n\n");
    prompt.push_str("**How markets work:**\n");
    prompt.push_str("- Each market asks: will this asset's price go UP or DOWN within a time window (15m/30m/1h)?\n");
    prompt.push_str("- You commit chips (virtual tokens) to your prediction. Winners get 1 chip per ticket. Losers get 0.\n");
    prompt.push_str("- Chips come from Chip Feed: 10,000 chips every 4 hours. Your current balance is all you have until the next feed.\n\n");

    prompt.push_str("**How pricing works (CLOB):**\n");
    prompt.push_str("- `implied_up_prob` is the market price, NOT a forecast. It reflects what other agents have already committed.\n");
    prompt.push_str("- When you buy UP at price 0.70, you pay 0.70 chips per ticket. If UP wins, you get 1.00 back (profit 0.30). If DOWN wins, you lose 0.70.\n");
    prompt.push_str("- When you buy DOWN at price 0.70 (meaning implied_up=0.70), you pay 0.30 per ticket. If DOWN wins, you get 1.00 (profit 0.70). If UP wins, you lose 0.30.\n");
    prompt.push_str("- The further the price is from 0.50, the worse the odds for the popular side and the better for the contrarian.\n\n");

    prompt.push_str("**How you earn $PRED rewards:**\n");
    prompt.push_str("- Participation Pool (20%): proportional to your number of submissions (capped at 300/day). More submissions = more participation reward.\n");
    prompt.push_str("- Alpha Pool (80%): proportional to your excess_score = max(0, balance - total_chips_fed_today). You earn Alpha only if you grew your chip balance beyond what was given.\n");
    prompt.push_str("- Implication: submit often for participation rewards, but be accurate and size well for Alpha rewards. Reckless large bets that lose destroy your Alpha score.\n\n");

    prompt.push_str("**Constraints:**\n");
    prompt.push_str("- 3 submissions per 15-minute timeslot. Use all 3 for participation, but pick the best opportunities.\n");
    prompt.push_str("- You can choose ANY market from the available list, not just the recommended one.\n\n");

    // Response format
    prompt.push_str("## Your Response\n\n");
    prompt.push_str("Output a JSON object with these fields:\n");
    prompt.push_str("- \"direction\": \"up\" or \"down\" — your prediction\n");
    prompt.push_str("- \"reasoning\": your analysis (80-2000 chars, ≥2 sentences, must mention the asset or a direction word like up/down/bullish/bearish). Must be original every time.\n");
    prompt.push_str("- \"tickets\": how many chips to commit (integer, ≥1)\n");
    prompt.push_str(&format!("- \"market_id\": which market (default: \"{}\")\n\n", market_id));
    prompt.push_str("Output ONLY the JSON. No markdown fences, no text outside the JSON.\n\n");

    // Current state
    prompt.push_str("## Your Current State\n\n");
    prompt.push_str(&format!("- Balance: {:.0} chips\n", balance));
    prompt.push_str(&format!("- Available markets: {}\n\n", all_markets.len()));

    // Recommended market
    prompt.push_str("## Recommended Market\n\n");
    prompt.push_str(&format!("- ID: {}\n", market_id));
    prompt.push_str(&format!("- Asset: {}\n", asset));
    prompt.push_str(&format!("- Window: {}\n", window));
    prompt.push_str(&format!("- Closes in: {}s\n", closes_in));
    prompt.push_str(&format!("- implied_up_prob: {:.2}\n", implied_up));
    // Explain the odds concretely
    if implied_up > 0.5 {
        prompt.push_str(&format!(
            "  → Buying UP costs {:.2}, profit if correct: {:.2}. Buying DOWN costs {:.2}, profit if correct: {:.2}.\n",
            implied_up, 1.0 - implied_up, 1.0 - implied_up, implied_up
        ));
    } else if implied_up < 0.5 {
        prompt.push_str(&format!(
            "  → Buying UP costs {:.2}, profit if correct: {:.2}. Buying DOWN costs {:.2}, profit if correct: {:.2}.\n",
            implied_up, 1.0 - implied_up, 1.0 - implied_up, implied_up
        ));
    } else {
        prompt.push_str("  → Fair odds (0.50/0.50). Your edge comes purely from analysis.\n");
    }
    prompt.push('\n');

    // Klines data
    if let Some(candles) = klines {
        if !candles.is_empty() {
            prompt.push_str(&format!("## Klines ({} candles)\n\n", candles.len()));
            prompt.push_str("time | open | high | low | close | volume\n");
            prompt.push_str("--- | --- | --- | --- | --- | ---\n");
            let start = if candles.len() > 20 { candles.len() - 20 } else { 0 };
            for candle in &candles[start..] {
                if let Some(obj) = candle.as_object() {
                    prompt.push_str(&format!(
                        "{} | {} | {} | {} | {} | {}\n",
                        obj.get("open_time").and_then(|v| v.as_i64()).unwrap_or(0),
                        obj.get("open").and_then(|v| v.as_f64()).map(|f| format!("{:.2}", f)).unwrap_or_default(),
                        obj.get("high").and_then(|v| v.as_f64()).map(|f| format!("{:.2}", f)).unwrap_or_default(),
                        obj.get("low").and_then(|v| v.as_f64()).map(|f| format!("{:.2}", f)).unwrap_or_default(),
                        obj.get("close").and_then(|v| v.as_f64()).map(|f| format!("{:.2}", f)).unwrap_or_default(),
                        obj.get("volume").and_then(|v| v.as_f64()).map(|f| format!("{:.0}", f)).unwrap_or_default(),
                    ));
                }
            }
            prompt.push('\n');
        } else {
            prompt.push_str("## Klines\n\nNo kline data available. Use market data and general market awareness.\n\n");
        }
    } else {
        prompt.push_str("## Klines\n\nNo kline data available. Use market data and general market awareness.\n\n");
    }

    // Other available markets
    if all_markets.len() > 1 {
        prompt.push_str("## Other Available Markets\n\n");
        for m in all_markets.iter().skip(1).take(8) {
            let m_implied = m["implied_up_prob"].as_f64().unwrap_or(0.5);
            prompt.push_str(&format!(
                "- {} ({} {}) implied_up={:.2} closes_in={}s\n",
                m["id"].as_str().unwrap_or("?"),
                m["asset"].as_str().unwrap_or("?"),
                m["window"].as_str().unwrap_or("?"),
                m_implied,
                m["closes_in_seconds"].as_i64().unwrap_or(0),
            ));
        }
        prompt.push_str("\nYou may choose a different market if you see better odds or stronger conviction.\n\n");
    }

    prompt
}

fn call_openclaw(openclaw_bin: &str, agent_id: &str, prompt: &str) -> Result<String> {
    // Purge sessions before calling to prevent context overflow
    let _ = Command::new(openclaw_bin)
        .args(["sessions", "purge", "--agent", agent_id, "--yes"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    // Write prompt to temp file to avoid shell escaping issues
    let tmp_path = std::env::temp_dir().join(format!("predict-prompt-{}.txt", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp_path)
            .context("failed to create temp prompt file")?;
        f.write_all(prompt.as_bytes())?;
    }

    // Read prompt from file and pipe to openclaw
    let prompt_content = std::fs::read_to_string(&tmp_path)?;

    let output = Command::new(openclaw_bin)
        .args(["agent", "--agent", agent_id, "--message", &prompt_content])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .context(format!("failed to execute openclaw at {}", openclaw_bin))?;

    // Clean up temp file
    let _ = std::fs::remove_file(&tmp_path);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output.status.code().unwrap_or(-1);
        // Check for rate limiting
        if stderr.contains("rate limit") || stderr.contains("429") {
            anyhow::bail!("OpenClaw rate limited (exit {}): {}", code, stderr.trim());
        }
        anyhow::bail!("openclaw failed (exit {}): {}", code, stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if stdout.trim().is_empty() {
        anyhow::bail!("openclaw returned empty response");
    }
    Ok(stdout)
}

fn parse_llm_response(text: &str) -> Result<(String, String, Option<u32>, Option<String>)> {
    // Try to extract JSON from the response
    // LLMs sometimes wrap JSON in markdown fences or add text around it
    let json_str = extract_json(text)
        .context("no JSON object found in LLM response")?;

    let v: Value = serde_json::from_str(&json_str)
        .context(format!("invalid JSON from LLM: {}", &json_str[..json_str.len().min(200)]))?;

    let direction = v
        .get("direction")
        .and_then(|d| d.as_str())
        .map(|s| s.to_lowercase())
        .filter(|s| s == "up" || s == "down")
        .context("missing or invalid 'direction' (must be 'up' or 'down')")?;

    let reasoning = v
        .get("reasoning")
        .and_then(|r| r.as_str())
        .map(|s| s.to_string())
        .filter(|s| s.len() >= 80)
        .context("missing or too short 'reasoning' (must be >= 80 chars)")?;

    let tickets = v
        .get("tickets")
        .and_then(|t| t.as_u64().or_else(|| t.as_f64().map(|f| f as u64)))
        .map(|t| t.max(1) as u32);

    let market_id = v
        .get("market_id")
        .and_then(|m| m.as_str())
        .map(|s| s.to_string());

    Ok((direction, reasoning, tickets, market_id))
}

/// Extract JSON object from text that may contain markdown fences or surrounding text.
fn extract_json(text: &str) -> Option<String> {
    let trimmed = text.trim();

    // Try parsing the whole thing first
    if trimmed.starts_with('{') {
        if serde_json::from_str::<Value>(trimmed).is_ok() {
            return Some(trimmed.to_string());
        }
    }

    // Try to find JSON inside markdown code fences
    if let Some(start) = trimmed.find("```json") {
        let after = &trimmed[start + 7..];
        if let Some(end) = after.find("```") {
            let candidate = after[..end].trim();
            if serde_json::from_str::<Value>(candidate).is_ok() {
                return Some(candidate.to_string());
            }
        }
    }
    if let Some(start) = trimmed.find("```") {
        let after = &trimmed[start + 3..];
        if let Some(end) = after.find("```") {
            let candidate = after[..end].trim();
            if candidate.starts_with('{') {
                if serde_json::from_str::<Value>(candidate).is_ok() {
                    return Some(candidate.to_string());
                }
            }
        }
    }

    // Find first { and last } and try parsing
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if end > start {
        let candidate = &trimmed[start..=end];
        if serde_json::from_str::<Value>(candidate).is_ok() {
            return Some(candidate.to_string());
        }
    }

    None
}

fn detect_openclaw() -> Option<String> {
    for name in &["openclaw", "openclaw.mjs", "openclaw.cmd"] {
        if which_exists(name) {
            return Some(name.to_string());
        }
    }
    // Check well-known paths
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = [
        format!("{home}/.local/bin/openclaw"),
        format!("{home}/.npm-global/bin/openclaw"),
        "/usr/local/bin/openclaw".to_string(),
    ];
    for path in &candidates {
        if std::path::Path::new(path).is_file() {
            return Some(path.clone());
        }
    }
    None
}

fn which_exists(name: &str) -> bool {
    let path_var = std::env::var("PATH").unwrap_or_default();
    path_var
        .split(':')
        .any(|dir| std::path::Path::new(dir).join(name).is_file())
}

fn ensure_agent(openclaw_bin: &str, agent_id: &str) {
    // Check if agent exists
    let check = Command::new(openclaw_bin)
        .args(["agents", "list"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output();

    if let Ok(output) = check {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.contains(agent_id) {
            log_debug!("loop: openclaw agent '{}' already exists", agent_id);
            return;
        }
    }

    // Create agent
    log_info!("loop: creating openclaw agent '{}'...", agent_id);
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let workspace = format!("{}/.openclaw/workspace-{}", home, agent_id);
    let result = Command::new(openclaw_bin)
        .args([
            "agents",
            "add",
            agent_id,
            "--workspace",
            &workspace,
            "--non-interactive",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .status();

    match result {
        Ok(status) if status.success() => {
            log_info!("loop: created openclaw agent '{}'", agent_id);
        }
        Ok(status) => {
            log_warn!(
                "loop: openclaw agent create exited with {} (may already exist)",
                status
            );
        }
        Err(e) => {
            log_warn!("loop: failed to create openclaw agent: {}", e);
        }
    }
}

fn calculate_backoff(base: u64, consecutive: u32, server_hint: Option<u64>) -> u64 {
    if let Some(hint) = server_hint {
        return hint;
    }
    // Exponential backoff: base * 2^consecutive, capped at 600s
    let multiplier = 2u64.pow(consecutive.min(4));
    (base * multiplier).min(600)
}

fn interruptible_sleep(seconds: u64, running: &Arc<AtomicBool>) {
    let end = Instant::now() + std::time::Duration::from_secs(seconds);
    while Instant::now() < end && running.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

fn extract_short_error(err: &str) -> String {
    if let Some(start) = err.find('{') {
        if let Ok(v) = serde_json::from_str::<Value>(&err[start..]) {
            if let Some(msg) = v
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
            {
                return msg.to_string();
            }
        }
    }
    err.chars().take(200).collect()
}
