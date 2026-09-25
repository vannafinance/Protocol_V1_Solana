# Vanna Protocol - Solana

Vanna is a lending and margin protocol for stock tokens on Solana: xStocks (listed stocks like TSLAx) and PreStocks (pre-IPO companies like OPENAI and ANTHROPIC).

- **Lend** stock tokens, SOL or USDC and earn interest.
- **Borrow** against them from one margin account to go long, go short, swap, or farm yield.

This repository contains the on-chain program, written with Anchor.

| | |
|---|---|
| **Live app** | <a href="https://devnet.solana.vanna.finance/portfolio" target="_blank" rel="noopener noreferrer">devnet.solana.vanna.finance/portfolio</a> |
| **Documentation** | <a href="https://docs.solana.vanna.finance" target="_blank" rel="noopener noreferrer">docs.solana.vanna.finance</a> |
| **Product walkthrough** | _Link coming soon_ |
| **Tech walkthrough** | <a href="https://www.youtube.com/watch?v=bF8RSiVm2JU" target="_blank" rel="noopener noreferrer">YouTube: Vanna Solana walkthrough</a> |


## Product walkthrough video

_Video coming soon._

<!-- When the video is ready, replace the line above with (VIDEO_ID = the part after watch?v=):
<a href="https://www.youtube.com/watch?v=VIDEO_ID" target="_blank" rel="noopener noreferrer"><img src="https://img.youtube.com/vi/VIDEO_ID/maxresdefault.jpg" alt="Vanna Solana product walkthrough" /></a>
-->

## Tech walkthrough video

<a href="https://www.youtube.com/watch?v=bF8RSiVm2JU" target="_blank" rel="noopener noreferrer"><img src="https://img.youtube.com/vi/bF8RSiVm2JU/maxresdefault.jpg" alt="Vanna Solana tech walkthrough" /></a>

## What the program does

| Area | What it does | Docs |
|---|---|---|
| **Earn** | Credit LPs supply SOL, USDC, TSLAx, GOOGLx, AAPLx, ANTHROPIC or OPENAI into per-token Credit LP pools and receive vTokens that grow with borrow interest (kink rate model). | <a href="https://docs.solana.vanna.finance/guides/earn/overview" target="_blank" rel="noopener noreferrer">Earn</a> |
| **Cross-margin account** | One margin PDA per wallet. Every collateral and debt position counts toward one health factor; liquidatable at HF ≤ 1.1. | <a href="https://docs.solana.vanna.finance/guides/margin/overview" target="_blank" rel="noopener noreferrer">Margin</a> |
| **Perps** | Long or short any supported stock token at up to 5×, built from borrowed tokens plus a Jupiter swap inside the margin account (no funding rate). | <a href="https://docs.solana.vanna.finance/guides/perps/overview" target="_blank" rel="noopener noreferrer">Perps</a> |
| **Farm** | Keep a stock as collateral, borrow USDC or SOL from Vanna at 1–5×, and supply it into Kamino's main market. | <a href="https://docs.solana.vanna.finance/guides/farm/overview" target="_blank" rel="noopener noreferrer">Farm</a> |
| **Swap** | Jupiter-routed swaps from the margin account, health-checked after the swap. | <a href="https://docs.solana.vanna.finance/guides/trade/spot-swap" target="_blank" rel="noopener noreferrer">Swap</a> |

Prices come from Pyth. Token-2022 transfer fees (PreStocks, 1%) are measured on every transfer, not assumed.

## Program

| | |
|---|---|
| Program ID | `BZ812nUv4Qhr2p1JVgmoJGjYTGk1brAXckyhFSCNH3Zg` |
| Framework | Anchor 1.1.2 (`anchor-lang`, `anchor-spl` with `token_2022`) |
| External programs | Jupiter v6, Kamino Lend, Pyth Receiver |

Full reference: <a href="https://docs.solana.vanna.finance/developers/contracts/program" target="_blank" rel="noopener noreferrer">Contract Reference</a> · <a href="https://docs.solana.vanna.finance/developers/math-reference" target="_blank" rel="noopener noreferrer">Math Reference</a> · <a href="https://docs.solana.vanna.finance/developers/deployed-contracts" target="_blank" rel="noopener noreferrer">Configured Accounts</a>

## Repository layout

```
programs/vanna_lending/src/
├── instructions/   admin, lending, margin, borrowing, composite, swap, lite, liquidation
├── state/          ProtocolConfig, AssetConfig, Reserve, MarginAccount, DebtPosition, Lite*
├── math/           fixed point, shares, interest, health
├── oracle/         Pyth price validation
├── validation/     account, position-scan and token-transfer checks
├── external/       Kamino CPI
└── tests/          LiteSVM integration tests
scripts/            TypeScript fork/devnet scripts + bootstrap-fork.sh
```

## Build and test

Requirements: Rust `1.89.0` (pinned in `rust-toolchain.toml`), Solana CLI 3.x, Anchor CLI `1.1.2`.

```bash
# Build the program (target/deploy/vanna_lending.so)
anchor build

# Unit tests (math, state)
anchor test
```



## Run on a local mainnet fork

```bash
# 1. Start Surfpool (clones mainnet accounts and deploys the program)
surfpool start --no-tui --no-studio -y --legacy-anchor-compatibility \
  --rpc-url https://api.mainnet-beta.solana.com --host 127.0.0.1 --port 8899 --ws-port 8900

# 2. Register assets, reserves and Kamino Lite strategies on the fresh fork
bash scripts/bootstrap-fork.sh
```

Then start the app from `Backend-Solana` and point Backpack at `http://127.0.0.1:8899`. Full steps: <a href="https://docs.solana.vanna.finance/guides/setup" target="_blank" rel="noopener noreferrer">Run and Setup</a> · <a href="https://docs.solana.vanna.finance/developers/setup" target="_blank" rel="noopener noreferrer">Developer Setup</a>.
