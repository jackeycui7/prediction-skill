# Predict WorkNet Skill

AWP Predict WorkNet 的 AI agent skill。Agent 分析加密资产 K 线数据，提交价格方向预测，赚取 $PRED 奖励。

## 依赖

- **predict-agent** — Rust CLI，与 Coordinator Server 交互
- **awp-wallet** — 签名和密钥管理

## 安装

### 1. 安装 awp-wallet

```bash
curl -sSL https://install.awp.sh/wallet | bash
awp-wallet setup
```

### 2. 安装 predict-agent

从 [releases](https://github.com/anthropics/predict-agent/releases) 下载预编译二进制：

```bash
# Linux x86_64
curl -sSL -o predict-agent https://releases.awp.sh/predict-agent/latest/predict-agent-linux-x86_64
chmod +x predict-agent
sudo mv predict-agent /usr/local/bin/
```

### 3. 配置环境

```bash
# 解锁 wallet（24 小时 session）
export AWP_WALLET_TOKEN=$(awp-wallet unlock --duration 86400 --scope full --raw)

# 可选：指定 coordinator URL（默认 https://api.predict.awp.sh）
export PREDICT_SERVER_URL=https://api.predict.awp.sh
```

### 4. 验证

```bash
predict-agent preflight
```

输出 `"status": "ready"` 即可。

## 工作原理

```
AWP Agent Runtime（每 2-3 分钟）
  → LLM 读 SKILL.md
  → predict-agent preflight    # 检查就绪
  → predict-agent context      # 拿市场 + K 线 + 推荐
  → LLM 分析 K 线，写 reasoning
  → predict-agent submit ...   # 提交预测
```

Agent 的所有操作通过 predict-agent CLI 完成。CLI 是编译后的 Rust 二进制，agent 无法修改其行为。

## 文件结构

```
prediction-skill/
└── SKILL.md          # LLM agent 指令文件
```

## 相关仓库

- [prediction server](https://github.com/jackeycui7/prediction) — Coordinator Server（Rust）
- [predict-agent](https://github.com/jackeycui7/prediction) — CLI 工具（Rust，在 predict-agent/ 目录）
