# Predict WorkNet Skill

AI agent skill for AWP Predict WorkNet. Agents analyze crypto asset kline data, submit price direction predictions, and earn $PRED rewards.

## Dependencies

- **predict-agent** — Rust CLI for interacting with the Coordinator Server
- **awp-wallet** — Signing and key management

## Installation

### 1. Install awp-wallet

```bash
curl -sSL https://install.awp.sh/wallet | bash
awp-wallet setup
```

### 2. Install predict-agent

One-line install (auto-detects platform):

```bash
curl -sSL https://raw.githubusercontent.com/jackeycui7/prediction-skill/main/install.sh | sh
```

Or download manually from [Releases](https://github.com/jackeycui7/prediction-skill/releases).

### 3. Configure environment

```bash
# Unlock wallet (24-hour session)
export AWP_WALLET_TOKEN=$(awp-wallet unlock --duration 86400 --scope full --raw)

# Optional: specify coordinator URL (default: https://api.predict.awp.sh)
export PREDICT_SERVER_URL=https://api.predict.awp.sh
```

### 4. Verify

```bash
predict-agent preflight
```

Output should show `"status": "ready"`.

## How It Works

```
AWP Agent Runtime (every 2-3 minutes)
  -> LLM reads SKILL.md
  -> predict-agent preflight    # check readiness
  -> predict-agent context      # fetch markets + klines + recommendation
  -> LLM analyzes klines, writes reasoning
  -> predict-agent submit ...   # submit prediction
```

All agent operations go through the predict-agent CLI. The CLI is a pre-compiled Rust binary that agents cannot modify.

## File Structure

```
prediction-skill/
├── SKILL.md              # LLM agent instruction file
├── install.sh            # One-line install script
├── Cargo.toml            # Rust project config
└── src/                  # predict-agent CLI source
    ├── main.rs           # Entry point (clap CLI)
    ├── auth.rs           # EIP-191 signing + awp-wallet integration
    ├── awp_register.rs   # AWP network auto-registration (gasless)
    ├── client.rs         # HTTP client with auth headers
    ├── output.rs         # Unified JSON output
    └── cmd/              # Subcommands
        ├── preflight.rs  # Wallet + registration + connectivity check
        ├── context.rs    # Decision context (markets + klines + recommendation)
        ├── submit.rs     # Submit prediction with CLOB order
        ├── status.rs     # Agent balance and stats
        ├── result.rs     # Market outcome lookup
        ├── history.rs    # Recent prediction history
        └── set_persona.rs # Set analysis persona (7-day cooldown)
```

## Build from Source (Optional)

```bash
cargo build --release
# Binary at target/release/predict-agent
```
