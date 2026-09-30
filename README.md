# Vanna Credit Layer — Solana

Vanna is a lending and margin protocol on Solana.

- **Lend** USDC, USDT or SOL and earn interest.
- **Borrow** them from one margin account against crypto, liquid-staked SOL, JupUSD or tokenized stocks (NVDAx, TSLAx), and use the borrowed funds in whitelisted external protocols (Kamino, Jupiter, GMTrade perps in any number of markets) through `margin_execute`, health-checked after every call.

The core program knows no price source and no external protocol. Two agent programs do: the **oracle** values what a margin holds, the **validator** decides which external calls it may make. Adding a protocol or a price source means a new file in the oracle or the validator, not a change to the core.

This repository contains the three programs (Anchor), their LiteSVM test suite, and the TypeScript scripts that drive them on a local mainnet fork.

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
- [Architecture: core and agents](#architecture-core-and-agents)
- [Run on a local mainnet fork](#run-on-a-local-mainnet-fork)
- [Script reference](#script-reference)
- [Assets](#assets)
- [What the program does](#what-the-program-does)
- [Repository layout](#repository-layout)

## Quick start

```bash
git clone https://github.com/vannafinance/vanna-credit-layer.git
cd vanna-credit-layer

anchor build                                                  # three programs -> target/deploy/*.so
cargo test --manifest-path programs/vanna_credit_layer/Cargo.toml  # full test suite
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

Builds the three programs into `target/deploy/`, with their IDLs in `target/idl/` and TypeScript types in `target/types/`. One program can be built with `cargo-build-sbf --manifest-path programs/<name>/Cargo.toml` and its IDL with `anchor idl build -p <name> -o target/idl/<name>.json`.

| Program | ID | Role |
|---|---|---|
| `vanna_credit_layer` | `BZ812nUv4Qhr2p1JVgmoJGjYTGk1brAXckyhFSCNH3Zg` | the core: pools, margin accounts, health, liquidation, `margin_execute` |
| `vanna_oracle` | `FXY5DRfekMUTbUp4uyCFpCZmCccrp6kPq3hTM4GRihnc` | the value of every holding: tokens from its price book (Kamino Scope, Pyth, Kamino cTokens), a margin's GMTrade venue account from its market book |
| `vanna_validator` | `6fND3vhtstp486iE6rsSNVUox3kcjNNRsSLAN7ZPs7th` | which external calls a margin may make: klend supply / redeem, exact-input Jupiter routes, GMTrade market orders in listed markets within leverage caps |

The IDs are the `declare_id!`s in each `lib.rs` and in `Anchor.toml`. If a local `target/deploy/<name>-keypair.json` belongs to a different address, build with:

```bash
anchor build --ignore-keys
```

Don't run `anchor keys sync` unless you mean to change the program ID: it rewrites `declare_id!` and `Anchor.toml`.

## Test

The tests run in LiteSVM (no validator) and load all three programs from `target/deploy/`, so **build first** and rebuild after any program change.

```bash
# Everything
cargo test --manifest-path programs/vanna_credit_layer/Cargo.toml

# Same, through Anchor (builds first)
anchor test
```

Each test binary can be run on its own with `--test <name>`:

| `--test` | Location | Covers |
|---|---|---|
| `kamino` | `src/tests/external/kamino/` | the validator's Kamino rules, cToken pricing, supply / redeem / withdraw, leverage and liquidation, refused calls, against the real klend program |
| `jupiter` | `src/tests/external/jupiter/` | the validator's Jupiter rules and swaps through the real Jupiter v6 and Orca programs |
| `gmtrade` | `src/tests/external/gmtrade/` | the validator's GMTrade permits (unit); longs and shorts opened, cancelled and closed through `margin_execute` against the real GMTrade program, in the ETH market and a second (BTC) market on one venue account; each market a tracked leg, valued on every health check and dropped at settlement; position equity vs. an independent model on the largest live mainnet positions; per-market leverage caps and trading switches, refusals, and unwinding then liquidating a venue account in one or two markets |
| `test_full_flow` | `src/tests/test_full_flow.rs` | lend → deposit → borrow → repay → withdraw |
| `test_liquidation` | `src/tests/test_liquidation.rs` | whole-account liquidation: every debt repaid, every asset swept, also below HF 1 |
| `test_security` | `src/tests/test_security.rs` | access control and account checks |
| `test_admin_and_lifecycle` | `src/tests/test_admin_and_lifecycle.rs` | admin instructions, account open / close |
| `test_interest_rate` | `src/tests/test_interest_rate.rs` | interest-rate model and accrual |
| `test_math` | `src/tests/test_math.rs` | fixed-point, share and health math |
| `test_state` | `src/tests/test_state.rs` | margin account position registries |
| `test_oracle` | `src/tests/test_oracle.rs` | token pricing through the oracle: Scope chains, Pyth fallback and factor, TWAP / staleness / confidence checks per instruction, pinned accounts, price-book admin rules; plus every asset priced from real mainnet Scope and Pyth accounts |
| `test_oracle_live` | `src/tests/test_oracle_live.rs` | **live, ignored by default**: reads the current mainnet oracle accounts, checks every asset's price (Kamino cTokens included) against Jupiter / Kamino market prices, the health factor of a margin account holding all ten assets on-chain against the off-chain one and the market's, and the oracle's equity of the largest live ETH positions |

```bash
M=programs/vanna_credit_layer/Cargo.toml

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
anchor build && cargo test --manifest-path programs/vanna_credit_layer/Cargo.toml
```

The `gmtrade` tests load the GMTrade store program, its ETH/USD market and store, the ETH and USDC oracles and the two largest live ETH positions, all read at one slot, from `src/tests/fixtures/gmtrade/` (refresh: `python3 scripts/dump-gmtrade-fixtures.py`). A second market ("BTC", the ETH market's account under its own address, market token and index token, priced by a Pyth feed the tests set) lets one venue account trade two markets of the real program. GMTrade's keepers execute orders off-chain, so the tests stand in for them by writing the executed position.

`test_oracle` loads Kamino's Scope prices account, the Pyth feed accounts and the eight mints, all read at one slot, from `src/tests/fixtures/oracles/` (clock pinned to that slot's block time). Refresh them with `python3 scripts/dump-oracle-fixtures.py`; the tests compare against the raw fixture data, so a refresh needs no test changes.

## Architecture: core and agents

Like the Solidity protocol's `RiskEngine` with an oracle per asset and a controller per protocol, the core only accounts: it knows what each margin holds and owes, and asks two agent programs the rest. They are registered by the admin, called read-only (no account writable, none signing) and answer through return data, which the core accepts only from the program it called. Their interface is `programs/vanna_credit_layer/src/interface.rs`.

- **Oracle** (`vanna_oracle`) implements `get_price(queries) -> Vec<PriceResult>`. Every asset names its oracle (`AssetConfig.oracle`). A query is a token amount (collateral or debt) or a venue holding; the answer is its USD value, the price checks it passed (fresh, TWAP, confidence), and for a venue which legs still hold exposure. The core values all of an account's holdings in one call.
  - `tokens.rs`: every token's price source is in its **price book** (`set_price_source`, the Solidity `setOracle`): a Kamino Scope chain first, Pyth as the fallback, a klend reserve's exchange rate for Kamino cTokens (`pricing.rs`, `scope.rs`, `pyth.rs`, `klend.rs`).
  - `gmtrade.rs`: a margin's **venue account** at GMTrade (below), from its **market book** (`open_market_book`, `list_market`).
- **Validator** (`vanna_validator`) implements `review_call(context) -> CallPermit`. Every whitelisted program names its validator (`Integration.validator`), and the validator picks its rules by the program called: `kamino.rs`, `jupiter.rs`, `gmtrade.rs`. The permit says who signs (the margin, or its venue account at a venue), which call accounts must be the margin or its venue account, which of the margin's vaults the call may pay into (`tokens_in`) and out of (`tokens_out`), which collateral funds the venue account, and which leg the call may open. Like the Solidity `exec` with `tokensIn` / `tokensOut`, the core then updates the active assets by balance: a vault paid into joins, a vault left empty leaves. It also guards every other account, refuses a vault left with a delegate or close authority, and health-checks the result.
- **Venues and venue accounts.** A venue (GMTrade, by its store) is an asset of kind `Venue`, settled in a token (USDC). A margin holds it through its venue account, the PDA `["venue_account", margin, venue]`: a system account that owns the margin's positions there and that Vanna signs for. A venue account trades every market of the venue. Each market is a **leg** (its index in the market book). The margin tracks, per venue, a bitmap of the legs that may hold exposure: an order the validator approves opens its leg, and `public_venue_settle` drops the legs the oracle reports empty. Every health check values every tracked leg, so exposure can't be hidden, and positions in many markets count toward one health factor.
- **Accounts.** An instruction that values the account or reviews a call ends its `remaining_accounts` with one segment per agent: the agent's program account, then the accounts it reads (its books, price accounts, a venue account's orders and positions), in any order.

Adding a protocol: add its rules as a file in the validator (and, if its positions aren't plain tokens, a file in the oracle), upgrade that program, then `admin_register_integration` (and `admin_register_venue` or `admin_register_asset`). The core is not changed or redeployed.

## Run on a local mainnet fork

```bash
# 1. Build, then start a Surfpool mainnet fork (from an empty directory: Surfpool writes files there)
anchor build
mkdir -p /tmp/vanna-fork && cd /tmp/vanna-fork
surfpool start --network mainnet --no-tui --no-deploy

# 2. In a second terminal: install the scripts' dependencies once
cd scripts && npm install

# 3. Deploy the three programs onto the fork (npm run deploy-fork), then register the assets, reserves,
#    the Kamino, Jupiter and GMTrade integrations and GMTrade's ETH, BTC and SOL markets
bash bootstrap-fork.sh
```

`deploy-fork` writes each program's loader accounts at its declared ID with Surfpool's `surfnet_setAccount`, so no deploy keypair is needed; re-run it after rebuilding. Some commands send v0 transactions through a lookup table they create (a health check of a margin with a venue account in several markets outgrows a legacy transaction).

The scripts use:

| Setting | Default | Override |
|---|---|---|
| RPC | `http://127.0.0.1:8899` | `DEVNET_RPC_URL=<url>` |
| Wallet | `~/.config/solana/id.json` | `ANCHOR_WALLET=<path>` or `--wallet <path>` |
| Jupiter API | `https://lite-api.jup.ag/swap/v1` | `JUPITER_API_URL=<url>` |
| Scope prices | Kamino's live `OraclePrices` account, copied from mainnet onto the fork before each command that values an account | `MAINNET_RPC_URL=<url>` |
| Pyth Hermes (Pyth fallbacks, JupUSD, the GMTrade BTC index) | `https://hermes.pyth.network`, no key | `PYTH_API_KEY=<key>` (required for real prices; without it the fork gets fixed fallback prices), `HERMES_URL=<url>` |
| Fallback prices (fork only, without a Hermes key) | `REFERENCE_PRICE` in `scripts/src/devnet-pyth.ts` | `FORK_PRICE_<FEED>=<usd>`, e.g. `FORK_PRICE_BTC=97000` to move a price and test liquidations |

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
npm run devnet -- borrow --asset usdc --amount 4000

# Supply 4,900 of the margin account's 5,000 USDC to Kamino (via margin_execute)
npm run integrations-fork -- kamino-deposit --symbol usdc --amount 4900

# Swap the other 100 USDC to SOL inside the margin account via Jupiter (via margin_execute;
# --dexes Whirlpool if the route hits an AMM that needs the real user to sign)
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

### Example: ETH, BTC and SOL on GMTrade with borrowed USDC, from one venue account

```bash
# Once per fork (bootstrap-fork.sh does it too): whitelist GMTrade, open the market book, list a
# market (it gets the next leg), register the venue. 5x caps.
npm run integrations-fork -- register-gmtrade --market eth --max-leverage 5
npm run integrations-fork -- register-gmtrade --market btc --max-leverage 5

# 1,000 USDC of margin, 1,000 more borrowed
npm run devnet -- deposit-collateral --asset usdc --amount 1000
npm run devnet -- borrow --asset usdc --amount 1000

# A 3x ETH long ($1,200 on 400 USDC) and a 3x BTC short ($900 on 300 USDC), 1% max slippage
npm run integrations-fork -- gmtrade-open --market eth --side long  --collateral 400 --size 1200
npm run integrations-fork -- gmtrade-open --market btc --side short --collateral 300 --size 900

# GMTrade's keepers fill orders on mainnet within seconds; on a fork, stand in for them
npm run integrations-fork -- gmtrade-simulate-fill --market eth --side long
npm run integrations-fork -- gmtrade-simulate-fill --market btc --side short

# The venue account: tracked legs, positions, PnL and equity per market; and the margin's health factor
npm run integrations-fork -- gmtrade-status
npm run devnet -- get-health-factor

# Close ETH and sweep its proceeds back into the margin account; BTC stays open and tracked
npm run integrations-fork -- gmtrade-close --market eth --side long
npm run integrations-fork -- gmtrade-simulate-fill --market eth --side long
npm run integrations-fork -- gmtrade-settle
```

A pending order can be cancelled with `gmtrade-cancel --market <m> --side <s>` (its USDC returns to the venue account; `gmtrade-settle` brings it back).

Liquidating a margin with an open venue account: `gmtrade-unwind --owner <pubkey> --market <m> --side <s>` for each open position (allowed only while the margin is liquidatable), let GMTrade fill the closes (`gmtrade-simulate-fill --owner <pubkey> ...` on a fork), then `devnet.ts liquidate --margin-owner <pubkey>`.

To use the app against the fork, start it from `Backend-Solana` and point Backpack at `http://127.0.0.1:8899`. Full steps: <a href="https://docs.solana.vanna.finance/guides/setup" target="_blank" rel="noopener noreferrer">Run and Setup</a> · <a href="https://docs.solana.vanna.finance/developers/setup" target="_blank" rel="noopener noreferrer">Developer Setup</a>.

## Script reference

Run either CLI with no command to print its command list.

**`npm run devnet -- <command>`** (`scripts/src/devnet.ts`): one command per program instruction, plus read-only queries.

| Group | Commands |
|---|---|
| Admin | `initialize-protocol`, `register-asset` (price source into the oracle's price book, then the asset), `set-asset-oracle` (the price source only), `update-asset-config`, `update-reserve-config`, `set-operating-mode`, `propose-authority`, `accept-admin`, `collect-protocol-fees` |
| Lending pool | `supply-liquidity`, `redeem-liquidity`, `refresh-reserve` |
| Margin account | `create-margin`, `close-margin`, `deposit-collateral`, `withdraw-collateral`, `reclaim-rent` |
| Borrowing | `borrow`, `repay-from-margin` |
| Liquidation | `liquidate --margin-owner <pubkey>`: repays every debt from the wallet and sweeps every asset (incl. Kamino cTokens) to it |
| Queries | `get-protocol-config`, `get-asset-config`, `get-reserve`, `get-margin-account`, `get-debt-position`, `get-balance`, `get-margin-vault-balance`, `get-share-balance`, `get-health-factor`, `get-position-summary` |

Assets (`--asset`): `usdc`, `usdt`, `wsol` (alias `sol`), `jitosol`, `jupsol`, `jupusd`, `nvdax`, `tslax`. `register-asset` puts the asset's price source (`ASSET_ORACLES` in `scripts/src/devnet-env.ts`) in the oracle's price book (opening the book the first time) and registers the asset valued by the oracle, making only the pool assets borrowable; `set-asset-oracle` re-applies that price source; `initialize-reserve` refuses anything but USDC, USDT and SOL.

**`npm run integrations-fork -- <command>`** (`scripts/src/integrations-fork.ts`): fork setup and external integrations.

| Group | Commands |
|---|---|
| Fork setup | `npm run deploy-fork` (`src/deploy-fork.ts`): the three programs at their IDs |
| Funding (fork only) | `fund --asset <a>`, `fund-sol` (native SOL), `fund-usdc`, all with `--to <pubkey> --amount <n>` |
| Kamino | `register-kamino`, `set-kamino-enabled`, `register-receipt --symbol <s>`, `kamino-deposit` / `kamino-redeem --symbol <s> --amount <n>` |
| Jupiter | `register-jupiter`, `jupiter-swap --from <a> --to <a> --amount <n> [--slippage-bps <bps>] [--dexes <amms>]` |
| GMTrade | `register-gmtrade --market <m> [--max-leverage <x>] [--trading on\|off]`; with `--market <m> --side long\|short`: `gmtrade-open --collateral <usdc> --size <usd> [--slippage-bps <bps> \| --acceptable-price <usd>]`, `gmtrade-close [--size <usd>] [--withdraw <usdc>]`, `gmtrade-cancel`; for the whole venue account: `gmtrade-settle [--owner <pubkey>]`, `gmtrade-status [--owner <pubkey>]`; fork only: `gmtrade-simulate-fill --market <m> --side <s> [--price <usd>] [--owner <pubkey>]`; liquidators: `gmtrade-unwind --owner <pubkey> --market <m> --side <s>` |

Kamino symbols (`--symbol`): `usdc`, `sol` (main market). GMTrade markets (`--market`, default `eth`): `eth`, `btc`, `sol` (the pure-USDC markets in `GM_MARKETS`, `scripts/src/gmtrade.ts`; another is one more entry there and a `register-gmtrade`).

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
| GMTrade (venue) | Positions in any listed market, held through the margin's venue account and traded through `margin_execute` only | — (keyed by the GMTrade store) | the venue account's equity over its tracked markets, valued by the oracle; index prices: ETH from Scope `3NJYft…` #246 (most recent of Chainlink and Pyth Lazer; TWAP #53), BTC from Pyth, SOL from Scope #3 | `Crypto.ETH/USD`, `Crypto.SOL/USD` (BTC: Pyth only) |

Every price but JupUSD's comes from Kamino's Scope aggregator (`3t4JZcueEzTbVP6kLxXrL3VpWx45jDer4eqysweBchNH`), whose keepers refresh these entries about every 40 s and which cross-checks Chainlink against Pyth Lazer; these are the entries Kamino's own reserves use, with Kamino's max ages (120–300 s) and TWAP limits (3–10%). The Chainlink xStocks price already applies the mint's Scaled UI multiplier, so it prices one raw token. Pyth fallbacks are the push oracle's own feed accounts, which Pyth updates every 55 s or on a 0.5% move. Mints, feed ids and Scope entries are in `scripts/src/devnet-env.ts` (`ASSET_ORACLES`) and `programs/vanna_oracle/src/reference.rs`.

### Perps on GMTrade

GMTrade (GMX-Solana) positions are accounts, not tokens, so GMTrade is a venue: the margin holds it through its venue account, the way the Solidity protocol holds a perp TrackToken.

- **Venue account.** Each margin trades every GMTrade market through one venue account, the PDA `["venue_account", margin, store]`. It is a plain system account (GMTrade requires one as owner) that owns the margin's GMTrade user, orders and positions, and Vanna signs for it in `margin_execute`. Positions are GMTrade PDAs derived from it (`["position", store, venue_account, market token, USDC, side]`), and each (market, side) has one order slot (nonce `[1 long / 2 short, leg, 0…]`), so every position and pending order is found without an index.
- **Markets.** The oracle's market book lists the markets Vanna accepts (`list_market`: the market, its index price, a leverage cap, a trading switch); a market's position in the book is its leg. The book is USDC-collateralized; ETH, BTC and SOL are listed by the scripts.
- **Trading.** The validator's GMTrade rules allow `prepare_user`, `prepare_position`, market increase / decrease `create_order_v2`, `close_order_v2` (cancel) and `close_empty_position`, signed by the venue account: listed markets only, USDC collateral, no swap path, trigger or callback, output and refunds paid back to the venue account, each order in its market and side's slot, within the market's leverage cap. An increase needs the market's trading switch on and opens its leg; its USDC moves from the margin's USDC vault into the venue account just before the call. A decrease needs its leg open. The venue's `collateral_enabled` switches new exposure off for every market; reducing and closing stay allowed.
- **Valuation.** Per margin, over the tracked legs: the venue account's idle USDC + per leg, USDC escrowed by pending orders + per position (collateral + PnL at the index price − pending borrowing and funding fees from the market's own cumulative factors − the close fee), floored at zero. Index and USDC prices carry the same freshness and TWAP checks as every other price.
- **Settling.** Anyone may call `public_venue_settle`: it sweeps the venue account's idle USDC (decrease output, refunds) into the margin's USDC vault, stops tracking legs with nothing open, and drops the venue from the active list once no leg is.
- **Liquidation.** A position can't change hands, so a liquidatable margin is liquidated in two steps: anyone unwinds its positions with `public_venue_unwind` (the validator, in unwind mode, allows only a market decrease of a whole position with no price limit), GMTrade executes the closes within seconds, and `public_liquidate` then pays the venue account's USDC out with every other asset. `public_liquidate` refuses while any leg holds a position or a pending order.

Orders are executed by GMTrade's keepers a few seconds after `margin_execute`; on a local fork they don't run, and `gmtrade-simulate-fill` stands in for them. A `margin_execute` into GMTrade needs about 250k compute units plus ~50k per tracked market, and a v0 transaction with a lookup table; the venue account pays GMTrade's rent and execution fee, so the client funds it with lamports in the same transaction (the scripts do both).

### How prices are read

Like the Solidity `OracleFacade`, one function prices every token: the oracle's `get_price` (`programs/vanna_oracle/src/pricing.rs`) reads the token's `OracleConfig` from the price book (set by `set_price_source`, the Solidity `setOracle`) and returns a price plus the checks it passed. The oracle uses the same function for GMTrade index prices.

- **Source.** The Scope chain (product of up to 4 entries) while it is fresh; otherwise the Pyth feed (× an optional second feed) if it is newer. A zero Scope entry counts as unavailable.
- **Checks.** Fresh (≤ `max_age_secs`), within `max_twap_divergence_bps` of its TWAP (Scope's TWAP chain, or Pyth's EMA), Pyth confidence within `max_confidence_bps`.
- **What each instruction requires**, as in Kamino: borrowing, and withdrawing or `margin_execute` while in debt, need every check on every holding of the account; liquidation needs only fresh prices, so a TWAP or confidence alarm in a crash never blocks it; a debt-free withdrawal reads no price at all (Solidity `isWithdrawAllowed`).

Every oracle account is pinned in the token's config, and `set_price_source` checks each one (owner, Pyth account = the feed's shard-0 account, a receipt uses its underlying's sources, which must be in the book with matching decimals) and prices the token once; `admin_register_asset` has the oracle price it once more.

**Accounts.** Instructions that value an account take, in `remaining_accounts`: the position groups (collateral `[asset_config, margin_vault]`, or `[asset_config, venue_account]` for a venue; debt `[asset_config, reserve, debt_position]`; in the margin's slot order), then (for `public_liquidate`, after the settlement accounts) the agent segments: the oracle's (its price book, every price account the tokens read, a cToken's klend reserve included, and when a venue account is held its market book, the venue account's USDC ATA and per tracked leg its market, index price accounts, order slots, escrows and positions). A `margin_execute` takes every position group (the tokens it spends and receives included), then one `[asset_config, margin_vault]` group per token the call brings in for the first time (`new_assets`), and the client creates that vault (its ATA) before the call; a GMTrade call marks the margin's USDC vault writable, as the core funds the venue account from it. A `margin_execute` or `public_venue_unwind` adds the validator's segment (for GMTrade, the market book). Agents find accounts by key; one that is needed but missing fails the instruction. `scripts/src/devnet-positions.ts` builds these lists.

## What the program does

| Area | What it does | Docs |
|---|---|---|
| **Earn** | Lenders supply USDC, USDT or SOL into per-token pools and receive vTokens that grow with borrow interest (polynomial utilization rate model). | <a href="https://docs.solana.vanna.finance/guides/earn/overview" target="_blank" rel="noopener noreferrer">Earn</a> |
| **Cross-margin account** | One margin PDA per wallet. Every collateral and debt position counts toward one health factor. At HF ≤ 1.10 (also below 1) anyone can liquidate the whole account, as in the Solidity AccountManager: repay every debt, receive every asset, Kamino cTokens included, in one transaction. | <a href="https://docs.solana.vanna.finance/guides/margin/overview" target="_blank" rel="noopener noreferrer">Margin</a> |
| **External calls** | `margin_execute` calls a whitelisted program (Kamino Lend, Jupiter, GMTrade) with the margin account, or at a venue its venue account, as signer. The validator reviews the call, the core enforces its permit, and the result is health-checked. See [Architecture](#architecture-core-and-agents). | — |
| **Perp markets** | Long or short ETH, BTC, SOL (any listed GMTrade market) with USDC from the margin account (borrowed or not), up to each market's leverage cap, all from one venue account; every position counts toward the health factor at its live equity. See [Perps on GMTrade](#perps-on-gmtrade). | — |
| **Perps** | Long any collateral asset (e.g. NVDAx, TSLAx, JitoSOL) at up to 5× by borrowing USDC / USDT / SOL and swapping through Jupiter inside the margin account (no funding rate). | <a href="https://docs.solana.vanna.finance/guides/perps/overview" target="_blank" rel="noopener noreferrer">Perps</a> |
| **Farm** | Borrow USDC or SOL from Vanna at 1–5× and supply it into Kamino; the cTokens stay in the margin account as collateral, valued at Kamino's exchange rate. | <a href="https://docs.solana.vanna.finance/guides/farm/overview" target="_blank" rel="noopener noreferrer">Farm</a> |
| **Swap** | Jupiter-routed swaps from the margin account, health-checked on post-trade balances. | <a href="https://docs.solana.vanna.finance/guides/trade/spot-swap" target="_blank" rel="noopener noreferrer">Swap</a> |

Prices come from Kamino's Scope aggregator (Chainlink and Pyth Lazer), with Pyth as the fallback (see [Assets](#assets)). Token-2022 mints are supported; any transfer fee is measured on every transfer, not assumed.

| | |
|---|---|
| Program IDs | core `BZ812nUv4Qhr2p1JVgmoJGjYTGk1brAXckyhFSCNH3Zg`; agents in [Build](#build) |
| Framework | Anchor 1.1.2 (`anchor-lang`, `anchor-spl` with `token_2022`) |
| External programs | Kamino Lend, Jupiter v6, GMTrade (GMX-Solana store); price accounts of Kamino Scope and the Pyth Receiver (read only) |

Full reference: <a href="https://docs.solana.vanna.finance/developers/contracts/program" target="_blank" rel="noopener noreferrer">Contract Reference</a> · <a href="https://docs.solana.vanna.finance/developers/math-reference" target="_blank" rel="noopener noreferrer">Math Reference</a> · <a href="https://docs.solana.vanna.finance/developers/deployed-contracts" target="_blank" rel="noopener noreferrer">Configured Accounts</a>

## Repository layout

```
programs/
├── vanna_credit_layer/src/     the core
│   ├── instructions/
│   │   ├── admin/              protocol, assets and venues, reserves, integrations
│   │   ├── lending_pool.rs     lender supply / redeem, interest refresh
│   │   └── account_manager/    account, collateral, borrow (borrow / repay), exec (margin_execute),
│   │                           venue account (settle, unwind), liquidate
│   ├── interface.rs            the core ↔ oracle / validator interface: price queries and results,
│   │                           call context and permits, venue account address, calling the agents
│   ├── risk_engine.rs          finds and validates every holding of a margin; has each valued
│   ├── state/                  ProtocolConfig, AssetConfig, Reserve, MarginAccount (incl. venue
│   │                           legs), DebtPosition, Integration
│   ├── math/                   fixed point, shares, interest-rate model, health factor
│   ├── validation/             account and token-transfer checks
│   └── tests/                  LiteSVM unit and integration tests (all three programs)
│       ├── external/kamino/    supply, redeem, withdraw, leverage, refusals against the real klend
│       ├── external/jupiter/   swaps against the real Jupiter v6 and Orca programs
│       ├── external/gmtrade/   validator permits; one venue account in two markets against the real GMTrade
│       ├── fixtures/mainnet/   those programs and accounts (scripts/dump-mainnet-fixtures.py)
│       ├── fixtures/gmtrade/   GMTrade program, ETH market, oracles, live positions
│       │                       (scripts/dump-gmtrade-fixtures.py)
│       └── fixtures/oracles/   real Scope and Pyth accounts (scripts/dump-oracle-fixtures.py)
├── vanna_oracle/src/           the oracle
│   ├── lib.rs                  instructions: get_price, price book, market book
│   ├── tokens.rs               price book; token values
│   ├── gmtrade.rs              market book; venue account values
│   ├── gmtrade_accounts.rs     GMTrade account layouts, addresses, instruction encoding
│   ├── pricing.rs              get_price: source selection and checks
│   ├── price.rs, config.rs     a price, fixed point; a token's OracleConfig
│   ├── scope.rs, pyth.rs, klend.rs   the three price readers
│   └── reference.rs            reference mainnet addresses
└── vanna_validator/src/        the validator
    ├── lib.rs                  review_call: picks the rules by the program called
    ├── kamino.rs               klend supply / redeem
    ├── jupiter.rs              Jupiter routes
    └── gmtrade.rs              GMTrade orders
scripts/
├── src/devnet.ts               one command per core instruction
├── src/integrations-fork.ts    fork funding, Kamino, Jupiter and GMTrade via margin_execute
├── src/deploy-fork.ts          deploys the three programs onto a Surfpool fork
├── src/agents.ts               agent segments, price book
├── src/gmtrade.ts              GMTrade markets, venue account and slot addresses, instructions, valuation mirror
├── src/oracle.ts               the price reads mirrored for display; fork oracle refresh
├── bootstrap-fork.sh           deploys and registers everything on a fresh Surfpool fork
├── dump-mainnet-fixtures.py    refreshes the klend / Jupiter test fixtures from mainnet
├── dump-oracle-fixtures.py     refreshes the oracle test fixtures from mainnet
└── dump-gmtrade-fixtures.py    refreshes the GMTrade test fixtures from mainnet
```
