/// EIP-191 personal_sign authentication for predict-agent.
///
/// Signing modes (in priority order):
///   1. AWP_PRIVATE_KEY=0x{hex}   — direct ECDSA signing (dev/test)
///   2. AWP_DEV_MODE=true         — dev mode, no real signing (matches server dev bypass)
///   3. awp-wallet subprocess     — production (calls `awp-wallet sign-message ...`)

use anyhow::{bail, Context, Result};
use chrono::Utc;
use k256::ecdsa::{SigningKey, signature::hazmat::PrehashSigner};
use sha3::{Digest, Keccak256};
use std::path::PathBuf;

pub struct AuthHeaders {
    pub address: String,
    pub timestamp: String,
    pub signature: String,
}

/// Build auth headers for a request. Timestamp is freshly generated.
pub fn build_auth_headers(address: &str) -> Result<AuthHeaders> {
    let timestamp = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let signature = sign_message(address, &timestamp)?;
    Ok(AuthHeaders {
        address: address.to_string(),
        timestamp,
        signature,
    })
}

fn sign_message(address: &str, timestamp: &str) -> Result<String> {
    let addr_lower = address.to_lowercase();

    // Mode 1: direct private key
    if let Ok(pk_hex) = std::env::var("AWP_PRIVATE_KEY") {
        return sign_with_key(&pk_hex, &addr_lower, timestamp);
    }

    // Mode 2: dev mode bypass
    if std::env::var("AWP_DEV_MODE").as_deref() == Ok("true")
        || std::env::var("AWP_DEV_MODE").as_deref() == Ok("1")
    {
        return Ok("dev".to_string());
    }

    // Mode 3: awp-wallet subprocess
    sign_with_wallet(&addr_lower, timestamp)
}

fn sign_with_key(pk_hex: &str, addr_lower: &str, timestamp: &str) -> Result<String> {
    let pk_hex = pk_hex.strip_prefix("0x").unwrap_or(pk_hex);
    let pk_bytes = hex::decode(pk_hex).context("Invalid AWP_PRIVATE_KEY hex")?;
    let signing_key =
        SigningKey::from_slice(&pk_bytes).context("Invalid private key bytes")?;

    let message = format!(
        "AWP Predict WorkNet\nAddress: {}\nTimestamp: {}",
        addr_lower, timestamp
    );

    // EIP-191 personal_sign hash
    let msg_hash = personal_sign_hash(message.as_bytes());

    // Sign the prehash
    let (sig, recovery_id): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) =
        signing_key.sign_prehash(&msg_hash)?;

    // Build 65-byte signature: r || s || v (v = recovery_id + 27)
    let mut sig_bytes = [0u8; 65];
    sig_bytes[..64].copy_from_slice(&sig.to_bytes());
    sig_bytes[64] = recovery_id.to_byte() + 27;

    Ok(format!("0x{}", hex::encode(sig_bytes)))
}

fn sign_with_wallet(addr_lower: &str, timestamp: &str) -> Result<String> {
    let token = std::env::var("AWP_WALLET_TOKEN")
        .context("AWP_WALLET_TOKEN not set. Run: awp-wallet unlock --duration 86400")?;

    let message = format!(
        "AWP Predict WorkNet\nAddress: {}\nTimestamp: {}",
        addr_lower, timestamp
    );

    let wallet_bin = find_awp_wallet()?;
    let output = std::process::Command::new(&wallet_bin)
        .args([
            "sign-message",
            "--token",
            &token,
            "--message",
            &message,
        ])
        .output()
        .context("Failed to run awp-wallet")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("awp-wallet sign failed: {}", stderr);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // awp-wallet outputs JSON: {"signature": "0x..."}
    let v: serde_json::Value =
        serde_json::from_str(&stdout).context("awp-wallet returned invalid JSON")?;
    let sig = v["signature"]
        .as_str()
        .context("awp-wallet response missing 'signature' field")?;
    Ok(sig.to_string())
}

/// Get wallet address from awp-wallet or AWP_ADDRESS env var.
pub fn get_address() -> Result<String> {
    // Direct env var (dev/test)
    if let Ok(addr) = std::env::var("AWP_ADDRESS") {
        return Ok(addr.to_lowercase());
    }

    // Derive from private key
    if let Ok(pk_hex) = std::env::var("AWP_PRIVATE_KEY") {
        return derive_address_from_key(&pk_hex);
    }

    // awp-wallet subprocess
    get_address_from_wallet()
}

fn derive_address_from_key(pk_hex: &str) -> Result<String> {
    let pk_hex = pk_hex.strip_prefix("0x").unwrap_or(pk_hex);
    let pk_bytes = hex::decode(pk_hex).context("Invalid AWP_PRIVATE_KEY hex")?;
    let signing_key = SigningKey::from_slice(&pk_bytes).context("Invalid private key bytes")?;
    let verifying_key = signing_key.verifying_key();
    let point = verifying_key.to_encoded_point(false);
    let pubkey_bytes = &point.as_bytes()[1..]; // skip 0x04 prefix
    let hash = Keccak256::digest(pubkey_bytes);
    Ok(format!("0x{}", hex::encode(&hash[12..])))
}

fn get_address_from_wallet() -> Result<String> {
    let agent_id = std::env::var("AWP_AGENT_ID").unwrap_or_default();
    let token = std::env::var("AWP_WALLET_TOKEN").unwrap_or_default();

    let mut args = vec!["receive"];
    if !agent_id.is_empty() {
        args.extend_from_slice(&["--agent", &agent_id]);
    }
    if !token.is_empty() {
        args.extend_from_slice(&["--token", &token]);
    }

    let wallet_bin = find_awp_wallet()?;
    let output = std::process::Command::new(&wallet_bin)
        .args(&args)
        .output()
        .context("Failed to run awp-wallet")?;

    if !output.status.success() {
        bail!("awp-wallet is locked. Run: awp-wallet unlock --duration 86400 --scope full");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).context("awp-wallet returned invalid JSON")?;
    let addr = v["eoaAddress"]
        .as_str()
        .or_else(|| v["address"].as_str())
        .context("awp-wallet response missing 'eoaAddress' field")?;
    Ok(addr.to_lowercase())
}

fn personal_sign_hash(message: &[u8]) -> Vec<u8> {
    let prefix = format!("\x19Ethereum Signed Message:\n{}", message.len());
    let mut hasher = Keccak256::new();
    hasher.update(prefix.as_bytes());
    hasher.update(message);
    hasher.finalize().to_vec()
}

/// Find awp-wallet binary. Checks PATH first, then well-known install locations.
pub fn find_awp_wallet() -> Result<PathBuf> {
    // Check PATH
    if let Ok(path) = which("awp-wallet") {
        return Ok(path);
    }

    // Search well-known locations (learned from awp-skill)
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = [
        format!("{home}/.local/bin/awp-wallet"),
        format!("{home}/.npm-global/bin/awp-wallet"),
        format!("{home}/.yarn/bin/awp-wallet"),
        "/usr/local/bin/awp-wallet".to_string(),
        "/usr/bin/awp-wallet".to_string(),
    ];

    for path_str in &candidates {
        let path = PathBuf::from(path_str);
        if path.is_file() {
            return Ok(path);
        }
    }

    bail!(
        "awp-wallet not found in PATH or standard locations. \
         Install it: curl -sSL https://install.awp.sh/wallet | bash"
    )
}

/// Minimal which(1) implementation.
fn which(binary: &str) -> Result<PathBuf> {
    let path_var = std::env::var("PATH").unwrap_or_default();
    for dir in path_var.split(':') {
        let candidate = PathBuf::from(dir).join(binary);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!("{binary} not found in PATH")
}
