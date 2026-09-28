# Vanna Credit Layer — Solana

Vanna is a lending and margin protocol on Solana.

- **Lend** USDC, USDT or SOL and earn interest.
- **Borrow** them from one margin account against crypto, liquid-staked SOL, JupUSD or tokenized stocks (NVDAx, TSLAx), and use the borrowed funds in whitelisted external protocols (Kamino, Jupiter) through `margin_execute`, health-checked after every call.

This repository contains the on-chain program (Anchor), its LiteSVM test suite, and the TypeScript scripts that drive it on a local mainnet fork.

| | |
|---|---|
| **Live app** | <a href="https://devnet.solana.vanna.finance/portfolio" target="_blank" rel="noopener noreferrer">devnet.solana.vanna.finance/portfolio</a> |
| **Documentation** | <a href="https://docs.solana.vanna.finance" target="_blank" rel="noopener noreferrer">docs.solana.vanna.finance</a> |
| **Product walkthrough** | <a href="https://www.youtube.com/watch?v=RjFdNyfky1s" target="_blank" rel="noopener noreferrer">YouTube: Vanna Solana product walkthrough</a> |
| **Tech walkthrough** | <a href="https://www.youtube.com/watch?v=bF8RSiVm2JU" target="_blank" rel="noopener noreferrer">YouTube: Vanna Solana tech walkthrough</a> |

## Contents

- [Quick start](#quick-start)
- [Prerequisites](#prerequisites)
- [Build](#build)
- [Test](#test)
- [Run on a local mainnet fork](#run-on-a-local-mainnet-fork)
- [Script reference](#script-reference)
- [Assets](#assets)
- [What the program does](#what-the-program-does)
- [Repository layout](#repository-layout)

## Quick start

```bash
git clone https://github.com/vannafinance/vanna-credit-layer.git
cd vanna-credit-layer

anchor build                                                  # program -> target/deploy/vanna_lending.so
cargo test --manifest-path programs/vanna_lending/Cargo.toml  # full test suite
```

## Prerequisites

| Tool | Version | Needed for |
|---|---|---|
| Rust | `1.89.0` (installed automatically from `rust-toolchain.toml`) | build, tests |
| Solana CLI | 3.x | build (`cargo build-sbf`), refreshing fixtures |
| Anchor CLI | `1.1.2` | build |
| Node.js + npm | 20+ | `scripts/` |
| Surfpool | latest | local mainnet fork |
| Python | 3.9+ | refreshing mainnet fixtures only |

```bash
# Anchor 1.1.2 via avm
cargo install --git https://github.com/solana-foundation/anchor avm --force
avm install 1.1.2 && avm use 1.1.2
```

## Build

```bash
anchor build
```

Outputs `target/deploy/vanna_lending.so`, the IDL in `target/idl/vanna_lending.json` and TypeScript types in `target/types/`.

The program ID is `BZ812nUv4Qhr2p1JVgmoJGjYTGk1brAXckyhFSCNH3Zg` (`declare_id!` in `lib.rs`, `Anchor.toml`). If your local `target/deploy/vanna_lending-keypair.json` belongs to a different address, build with:

```bash
anchor build --ignore-keys
```

Don't run `anchor keys sync` unless you mean to change the program ID: it rewrites `declare_id!` and `Anchor.toml`.

## Test

The tests run in LiteSVM (no validator) and load `target/deploy/vanna_lending.so`, so **build first** and rebuild after any program change.

```bash
# Everything
cargo test --manifest-path programs/vanna_lending/Cargo.toml

# Same, through Anchor (builds first)
anchor test
```

Each test binary can be run on its own with `--test <name>`:

| `--test` | Location | Covers |
|---|---|---|
| `kamino` | `src/tests/external/kamino/` | Kamino adapter, cToken pricing, supply / redeem / withdraw, leverage and liquidation, refused calls, against the real klend program |
| `jupiter` | `src/tests/external/jupiter/` | Jupiter adapter and swaps through the real Jupiter v6 and Orca programs |
| `test_full_flow` | `src/tests/test_full_flow.rs` | lend → deposit → borrow → repay → withdraw |
| `test_liquidation` | `src/tests/test_liquidation.rs` | whole-account liquidation: every debt repaid, every asset swept, also below HF 1 |
| `test_security` | `src/tests/test_security.rs` | access control and account checks |
| `test_admin_and_lifecycle` | `src/tests/test_admin_and_lifecycle.rs` | admin instructions, account open / close |
| `test_interest_rate` | `src/tests/test_interest_rate.rs` | interest-rate model and accrual |
| `test_math` | `src/tests/test_math.rs` | fixed-point, share and health math |
| `test_state` | `src/tests/test_state.rs` | margin account position registries |
| `test_oracle` | `src/tests/test_oracle.rs` | the oracle facade: Scope chains, Pyth fallback and factor, TWAP / staleness / confidence checks per instruction, pinned accounts, admin rules; plus every asset priced from real mainnet Scope and Pyth accounts |
| `test_oracle_live` | `src/tests/test_oracle_live.rs` | **live, ignored by default**: reads the current mainnet oracle accounts, checks every asset's price (Kamino cTokens included) against Jupiter / Kamino market prices, and the health factor of a margin account holding all ten assets on-chain against the facade's and the market's |

```bash
M=programs/vanna_lending/Cargo.toml

cargo test --manifest-path $M --test kamino                  # one binary
cargo test --manifest-path $M --test kamino withdraw         # one module (name filter)
cargo test --manifest-path $M --test jupiter -- --nocapture  # show println! output

# 5x leveraged Kamino farm: prints expected vs. actual health factor, interest and yield per step
cargo test --manifest-path $M --test kamino leveraged_kamino_farm_walkthrough -- --nocapture

# Live prices and margin health against mainnet right now (network; MAINNET_RPC_URL to override)
cargo test --manifest-path $M --test test_oracle_live -- --ignored --nocapture
```

### Mainnet fixtures

The `kamino` and `jupiter` tests load real mainnet programs (klend, Jupiter v6, Orca Whirlpool) and accounts (Kamino reserves, an Orca SOL/USDC pool) from `src/tests/fixtures/mainnet/`. The clock is pinned to the snapshot's block time. To refresh the fixtures after one of those programs is upgraded:

```bash
python3 scripts/dump-mainnet-fixtures.py                # or: --rpc <mainnet RPC URL>
anchor build && cargo test --manifest-path programs/vanna_lending/Cargo.toml
```

`test_oracle` loads Kamino's Scope prices account, the Pyth feed accounts and the eight mints, all read at one slot, from `src/tests/fixtures/oracles/` (clock pinned to that slot's block time). Refresh them with `python3 scripts/dump-oracle-fixtures.py`; the tests compare against the raw fixture data, so a refresh needs no test changes.

## Run on a local mainnet fork

```bash
# 1. Build, then start Surfpool from the repo root (forks mainnet and deploys the program)
anchor build
surfpool start --no-tui --no-studio -y --legacy-anchor-compatibility \
  --rpc-url https://api.mainnet-beta.solana.com --host 127.0.0.1 --port 8899 --ws-port 8900

# 2. In a second terminal: install the scripts' dependencies once
cd scripts && npm install

# 3. Register assets, reserves, and the Kamino and Jupiter integrations on the fresh fork
bash bootstrap-fork.sh
```

The scripts use:

| Setting | Default | Override |
|---|---|---|
| RPC | `http://127.0.0.1:8899` | `DEVNET_RPC_URL=<url>` |
| Wallet | `~/.config/solana/id.json` | `ANCHOR_WALLET=<path>` or `--wallet <path>` |
| Jupiter API | `https://lite-api.jup.ag/swap/v1` | `JUPITER_API_URL=<url>` |
| Scope prices | Kamino's live `OraclePrices` account, copied from mainnet onto the fork before each command that values an account | `MAINNET_RPC_URL=<url>` |
| Pyth Hermes (Pyth fallbacks and JupUSD) | `https://hermes.pyth.network`, no key | `PYTH_API_KEY=<key>` (required for real prices; without it the fork gets fixed fallback prices), `HERMES_URL=<url>` |

The wallet that runs `bootstrap-fork.sh` becomes the protocol admin.

### Example: lend, borrow, farm on Kamino, swap on Jupiter, unwind

All commands run from `scripts/`. Amounts are in whole tokens (e.g. `100.5`).

```bash
WALLET=$(solana address)

# Fund the wallet on the fork
npm run integrations-fork -- fund-sol  --to $WALLET --amount 10
npm run integrations-fork -- fund-usdc --to $WALLET --amount 10000

# Lend USDC to the pool (receive vUSDC shares)
npm run devnet -- supply-liquidity --asset usdc --amount 5000

# Open a margin account, deposit collateral, borrow 4x
npm run devnet -- create-margin
npm run devnet -- deposit-collateral --asset usdc --amount 1000
npm run devnet -- open-debt-position --asset usdc
npm run devnet -- borrow --asset usdc --amount 4000

# Supply 4,900 of the margin account's 5,000 USDC to Kamino (via margin_execute)
npm run integrations-fork -- kamino-deposit --symbol usdc --amount 4900

# Swap the other 100 USDC to SOL inside the margin account via Jupiter (via margin_execute)
npm run integrations-fork -- jupiter-swap --from usdc --to wsol --amount 100 --slippage-bps 50

# Inspect
npm run devnet -- get-position-summary
npm run devnet -- get-health-factor

# Unwind: redeem cUSDC from Kamino, repay, withdraw to the wallet
npm run integrations-fork -- kamino-redeem --symbol usdc --amount <cUSDC amount>
npm run devnet -- repay-from-margin --asset usdc --repay-all
npm run devnet -- withdraw-collateral --asset usdc --amount <USDC amount>
npm run devnet -- withdraw-collateral --asset wsol --amount <SOL amount>
```

To use the app against the fork, start it from `Backend-Solana` and point Backpack at `http://127.0.0.1:8899`. Full steps: <a href="https://docs.solana.vanna.finance/guides/setup" target="_blank" rel="noopener noreferrer">Run and Setup</a> · <a href="https://docs.solana.vanna.finance/developers/setup" target="_blank" rel="noopener noreferrer">Developer Setup</a>.

## Script reference

Run either CLI with no command to print its command list.

**`npm run devnet -- <command>`** (`scripts/src/devnet.ts`): one command per program instruction, plus read-only queries.

| Group | Commands |
|---|---|
| Admin | `initialize-protocol`, `register-asset`, `set-asset-oracle`, `initialize-reserve`, `update-asset-config`, `update-reserve-config`, `set-operating-mode`, `propose-authority`, `accept-admin`, `collect-protocol-fees` |
| Lending pool | `supply-liquidity`, `redeem-liquidity`, `refresh-reserve` |
| Margin account | `create-margin`, `close-margin`, `deposit-collateral`, `withdraw-collateral`, `close-collateral-position` |
| Borrowing | `open-debt-position`, `borrow`, `repay-from-margin`, `repay-from-wallet`, `close-debt-position` |
| Liquidation | `liquidate --margin-owner <pubkey>`: repays every debt from the wallet and sweeps every asset (incl. Kamino cTokens) to it |
| Queries | `get-protocol-config`, `get-asset-config`, `get-reserve`, `get-margin-account`, `get-debt-position`, `get-balance`, `get-margin-vault-balance`, `get-share-balance`, `get-health-factor`, `get-position-summary` |

Assets (`--asset`): `usdc`, `usdt`, `wsol` (alias `sol`), `jitosol`, `jupsol`, `jupusd`, `nvdax`, `tslax`. `register-asset` registers the asset with its oracle (`ASSET_ORACLES` in `scripts/src/devnet-env.ts`) and makes only the pool assets borrowable; `set-asset-oracle` re-applies that oracle to a registered asset; `initialize-reserve` refuses anything but USDC, USDT and SOL.

**`npm run integrations-fork -- <command>`** (`scripts/src/integrations-fork.ts`): fork setup and external integrations.

| Group | Commands |
|---|---|
| Funding (fork only) | `fund --asset <a>`, `fund-sol` (native SOL), `fund-usdc`, all with `--to <pubkey> --amount <n>` |
| Kamino | `register-kamino`, `set-kamino-enabled`, `register-receipt --symbol <s>`, `kamino-deposit` / `kamino-redeem --symbol <s> --amount <n> [--min-received <n>]` |
| Jupiter | `register-jupiter`, `jupiter-swap --from <a> --to <a> --amount <n> [--slippage-bps <bps>] [--min-received <n>]` |

Kamino symbols (`--symbol`): `usdc`, `sol` (main market).

## Assets

| Asset | Role | Token program | Price (Scope entries) | Pyth fallback |
|---|---|---|---|---|
| USDC | Lending pool + collateral | SPL Token | #13: Chainlink / Pyth Lazer, capped at $1 | `Crypto.USDC/USD` |
| USDT | Lending pool + collateral | SPL Token | #16: Chainlink / Pyth Lazer, capped at $1 | `Crypto.USDT/USD` |
| SOL (wSOL) | Lending pool + collateral | SPL Token | #3: Chainlink / Pyth Lazer | `Crypto.SOL/USD` |
| JitoSOL | Collateral only | SPL Token | #210 × #3: stake-pool rate × SOL | — |
| JupSOL | Collateral only | SPL Token | #224 × #3: stake-pool rate × SOL | `Crypto.JUPSOL/SOL.RR` × `Crypto.SOL/USD` |
| JupUSD | Collateral only | SPL Token | — | `Crypto.JUPUSD/USD` (only source) |
| NVDAx | Collateral only | Token-2022 | #332: Chainlink xStocks / Pyth Lazer | — |
| TSLAx | Collateral only | Token-2022 | #338: Chainlink xStocks / Pyth Lazer | — |
| Kamino cUSDC / cSOL | Collateral only (from Kamino deposits) | SPL Token | the underlying's price, through the Kamino reserve's exchange rate | the underlying's |

Every price but JupUSD's comes from Kamino's Scope aggregator (`3t4JZcueEzTbVP6kLxXrL3VpWx45jDer4eqysweBchNH`), whose keepers refresh these entries about every 40 s and which cross-checks Chainlink against Pyth Lazer; these are the entries Kamino's own reserves use, with Kamino's max ages (120–300 s) and TWAP limits (3–10%). The Chainlink xStocks price already applies the mint's Scaled UI multiplier, so it prices one raw token. Pyth fallbacks are the push oracle's own feed accounts, which Pyth updates every 55 s or on a 0.5% move. Mints, feed ids and Scope entries are in `scripts/src/devnet-env.ts` (`ASSET_ORACLES`) and `programs/vanna_lending/src/constants.rs`.

### How prices are read

Like the Solidity `OracleFacade`, one function prices every asset: `oracle::get_price` reads the asset's `OracleConfig` (set by `admin_register_asset` / `admin_set_asset_oracle`, the Solidity `setOracle`) and returns a price plus the checks it passed:

- **Source.** The Scope chain (product of up to 4 entries) while it is fresh; otherwise the Pyth feed (× an optional second feed) if it is newer. A zero Scope entry counts as unavailable.
- **Checks.** Fresh (≤ `max_age_secs`), within `max_twap_divergence_bps` of its TWAP (Scope's TWAP chain, or Pyth's EMA), Pyth confidence within `max_confidence_bps`.
- **What each instruction requires**, as in Kamino: borrowing, and withdrawing or `margin_execute` while in debt, need every check on every asset of the account; liquidation needs only fresh prices, so a TWAP or confidence alarm in a crash never blocks it; a debt-free withdrawal reads no price at all (Solidity `isWithdrawAllowed`).

Every oracle account is pinned in the asset's config, and registration checks each one (owner, Pyth account = the feed's shard-0 account, a receipt uses its underlying's sources) and prices the asset once.

**Accounts.** Instructions that value an account take, in `remaining_accounts`: the position groups (collateral `[asset_config, margin_vault]`, debt `[asset_config, reserve, debt_position]`, in the margin's slot order), then every oracle account the involved assets read, each once, in any order (for `public_liquidate`, after the settlement accounts). The program finds oracle accounts by key; one that is needed but missing fails the instruction. `scripts/src/devnet-positions.ts` builds these lists.

## What the program does

| Area | What it does | Docs |
|---|---|---|
| **Earn** | Lenders supply USDC, USDT or SOL into per-token pools and receive vTokens that grow with borrow interest (polynomial utilization rate model). | <a href="https://docs.solana.vanna.finance/guides/earn/overview" target="_blank" rel="noopener noreferrer">Earn</a> |
| **Cross-margin account** | One margin PDA per wallet. Every collateral and debt position counts toward one health factor. At HF ≤ 1.10 (also below 1) anyone can liquidate the whole account, as in the Solidity AccountManager: repay every debt, receive every asset, Kamino cTokens included, in one transaction. | <a href="https://docs.solana.vanna.finance/guides/margin/overview" target="_blank" rel="noopener noreferrer">Margin</a> |
| **External calls** | `margin_execute` calls a whitelisted program (Kamino Lend, Jupiter) with the margin account as signer. A per-protocol adapter validates the call, and the result is health-checked. | — |
| **Perps** | Long any collateral asset (e.g. NVDAx, TSLAx, JitoSOL) at up to 5× by borrowing USDC / USDT / SOL and swapping through Jupiter inside the margin account (no funding rate). | <a href="https://docs.solana.vanna.finance/guides/perps/overview" target="_blank" rel="noopener noreferrer">Perps</a> |
| **Farm** | Borrow USDC or SOL from Vanna at 1–5× and supply it into Kamino; the cTokens stay in the margin account as collateral, valued at Kamino's exchange rate. | <a href="https://docs.solana.vanna.finance/guides/farm/overview" target="_blank" rel="noopener noreferrer">Farm</a> |
| **Swap** | Jupiter-routed swaps from the margin account, health-checked on post-trade balances. | <a href="https://docs.solana.vanna.finance/guides/trade/spot-swap" target="_blank" rel="noopener noreferrer">Swap</a> |

Prices come from Kamino's Scope aggregator (Chainlink and Pyth Lazer), with Pyth as the fallback (see [Assets](#assets)). Token-2022 mints are supported; any transfer fee is measured on every transfer, not assumed.

| | |
|---|---|
| Program ID | `BZ812nUv4Qhr2p1JVgmoJGjYTGk1brAXckyhFSCNH3Zg` |
| Framework | Anchor 1.1.2 (`anchor-lang`, `anchor-spl` with `token_2022`) |
| External programs | Kamino Lend, Jupiter v6; price accounts of Kamino Scope and the Pyth Receiver (read only) |

Full reference: <a href="https://docs.solana.vanna.finance/developers/contracts/program" target="_blank" rel="noopener noreferrer">Contract Reference</a> · <a href="https://docs.solana.vanna.finance/developers/math-reference" target="_blank" rel="noopener noreferrer">Math Reference</a> · <a href="https://docs.solana.vanna.finance/developers/deployed-contracts" target="_blank" rel="noopener noreferrer">Configured Accounts</a>

## Repository layout

```
programs/vanna_lending/src/
├── instructions/
│   ├── admin/            protocol, assets (incl. their oracles), reserves, integrations
│   ├── lending_pool.rs   lender supply / redeem, interest refresh
│   └── account_manager/  account, collateral, borrow (borrow / repay), exec (margin_execute),
│                         liquidate
├── adapters/       per-protocol call validators for margin_execute (Kamino Lend, Jupiter)
├── risk_engine.rs  finds, validates and values every position of a margin account
├── oracle/         the oracle facade (get_price): Scope and Pyth readers, price checks,
│                   Kamino cToken exchange rate
├── state/          ProtocolConfig, AssetConfig, Reserve, MarginAccount, DebtPosition, Integration
├── math/           fixed point, shares, interest-rate model, health factor
├── validation/     account and token-transfer checks
└── tests/          LiteSVM unit and integration tests
    ├── external/kamino/   supply, redeem, withdraw, leverage, refusals against the real klend
    ├── external/jupiter/  swaps against the real Jupiter v6 and Orca programs
    ├── fixtures/mainnet/  those programs and accounts (scripts/dump-mainnet-fixtures.py)
    └── fixtures/oracles/  real Scope and Pyth accounts (scripts/dump-oracle-fixtures.py)
scripts/
├── src/devnet.ts             one command per program instruction
├── src/integrations-fork.ts  fork funding, Kamino and Jupiter via margin_execute
├── src/oracle.ts             the program's price reads mirrored for display; fork oracle refresh
├── bootstrap-fork.sh         registers everything on a fresh Surfpool fork
├── dump-mainnet-fixtures.py  refreshes the klend / Jupiter test fixtures from mainnet
└── dump-oracle-fixtures.py   refreshes the oracle test fixtures from mainnet
```
