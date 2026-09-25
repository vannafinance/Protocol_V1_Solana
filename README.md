# Vanna Protocol - Solana

Vanna turns listed and pre-IPO stock tokens on Solana (xStocks and PreStocks) into borrowable, yield-bearing collateral. Supply them to earn interest, and long, short, swap or farm them from one cross-margin account.

This repository holds the on-chain Anchor program. The app and the docs live in their own repositories (links below).

| | |
|---|---|
| **Live app** | [devnet.solana.vanna.finance/portfolio](https://devnet.solana.vanna.finance/portfolio) |
| **Documentation** | [docs.solana.vanna.finance](https://docs.solana.vanna.finance) |
| **Tech walkthrough** | [YouTube: Vanna Solana walkthrough](https://www.youtube.com/watch?v=bF8RSiVm2JU) |
| **Fork RPC** | `https://rpc-devnet.solana.vanna.finance` ([wallet setup](https://docs.solana.vanna.finance/guides/connect-wallet#set-a-custom-rpc-in-backpack)) |


## Walkthrough video

[![Vanna Solana tech walkthrough](https://img.youtube.com/vi/bF8RSiVm2JU/maxresdefault.jpg)](https://www.youtube.com/watch?v=bF8RSiVm2JU)

## Screenshots

| Earn | Perps - Long |
|---|---|
| ![Earn pools](https://docs.solana.vanna.finance/images/earn/overview.png) | ![Perps long](https://docs.solana.vanna.finance/images/perps/long.png) |
| **Perps - Short** | **Farm - One-Click** |
| ![Perps short](https://docs.solana.vanna.finance/images/perps/short.png) | ![Farm one-click](https://docs.solana.vanna.finance/images/farm/one-click.png) |
| **Farm - Position** | **Swap** |
| ![Farm position](https://docs.solana.vanna.finance/images/farm/position.png) | ![Swap](https://docs.solana.vanna.finance/images/swap/overview.png) |
| **Earn - Supply** | **Faucet** |
| ![Earn supply](https://docs.solana.vanna.finance/images/earn/supply.png) | ![Faucet](https://docs.solana.vanna.finance/images/onboarding/faucet.png) |

## What the program does

| Area | What it does | Docs |
|---|---|---|
| **Earn** | Credit LPs supply SOL, USDC, TSLAx, GOOGLx, AAPLx, ANTHROPIC or OPENAI into per-token Credit LP pools and receive vTokens that grow with borrow interest (kink rate model). | [Earn](https://docs.solana.vanna.finance/guides/earn/overview) |
| **Cross-margin account** | One margin PDA per wallet. Every collateral and debt position counts toward one health factor; liquidatable at HF ≤ 1.1. | [Margin](https://docs.solana.vanna.finance/guides/margin/overview) |
| **Perps** | Long or short any supported stock token at up to 5×, built from borrowed tokens plus a Jupiter swap inside the margin account (no funding rate). | [Perps](https://docs.solana.vanna.finance/guides/perps/overview) |
| **Farm** | Keep a stock as collateral, borrow USDC or SOL from Vanna at 1–5×, and supply it into Kamino's main market. | [Farm](https://docs.solana.vanna.finance/guides/farm/overview) |
| **Swap** | Jupiter-routed swaps from the margin account, health-checked after the swap. | [Swap](https://docs.solana.vanna.finance/guides/trade/spot-swap) |

Prices come from Pyth. Token-2022 transfer fees (PreStocks, 1%) are measured on every transfer, not assumed.

## Program

| | |
|---|---|
| Program ID | `BZ812nUv4Qhr2p1JVgmoJGjYTGk1brAXckyhFSCNH3Zg` |
| Framework | Anchor 1.1.2 (`anchor-lang`, `anchor-spl` with `token_2022`) |
| External programs | Jupiter v6, Kamino Lend, Pyth Receiver |

| Instruction group | Instructions |
|---|---|
| Admin | `initialize_protocol`, `admin_propose_authority`, `authority_accept_admin`, `admin_set_operating_mode`, `admin_register_asset`, `admin_update_asset_config`, `admin_initialize_reserve`, `admin_update_reserve_config`, `admin_collect_protocol_fees`, `admin_register_lite_strategy` |
| Lending | `lender_supply`, `lender_redeem`, `public_refresh_reserve` |
| Margin | `user_create_margin`, `user_close_margin`, `user_deposit_collateral`, `user_withdraw_collateral`, `user_close_collateral_position` |
| Borrowing | `user_open_debt_position`, `user_close_debt_position`, `user_borrow`, `user_deposit_and_borrow`, `user_repay_from_margin`, `public_repay_from_wallet` |
| Swap | `user_margin_swap` |
| Farm (Lite / Kamino) | `lite_open`, `lite_supply`, `lite_reduce_redeem`, `lite_reduce_repay`, `lite_reduce_and_repay` |
| Liquidation | `public_liquidate` |

Full reference: [Contract Reference](https://docs.solana.vanna.finance/developers/contracts/program) · [Math Reference](https://docs.solana.vanna.finance/developers/math-reference) · [Configured Accounts](https://docs.solana.vanna.finance/developers/deployed-contracts)

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

Then start the app from `Backend-Solana` and point Backpack at `http://127.0.0.1:8899`. Full steps: [Run and Setup](https://docs.solana.vanna.finance/guides/setup) · [Developer Setup](https://docs.solana.vanna.finance/developers/setup).
