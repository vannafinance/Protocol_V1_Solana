#!/usr/bin/env bash
# Fresh Surfpool bootstrap for Vanna (SOL + USDC + TSLAx + GOOGLx + AAPLx + ANTHROPIC + OPENAI +
# Kamino lite). Prereq: surfpool running on 127.0.0.1:8899 with the program auto-deployed.
set -euo pipefail
export DEVNET_RPC_URL="${DEVNET_RPC_URL:-http://127.0.0.1:8899}"
cd "$(dirname "$0")"
echo "==> RPC $DEVNET_RPC_URL"
npx tsx src/devnet.ts initialize-protocol || true
npx tsx src/devnet.ts register-asset --asset wsol || true
npx tsx src/devnet.ts initialize-reserve --asset wsol || true
npx tsx src/devnet.ts register-asset --asset usdc || true
npx tsx src/devnet.ts initialize-reserve --asset usdc || true
npx tsx src/devnet.ts register-asset --asset anthropic || true
npx tsx src/devnet.ts initialize-reserve --asset anthropic || true
npx tsx src/devnet.ts register-asset --asset openai || true
npx tsx src/devnet.ts initialize-reserve --asset openai || true
npx tsx src/lite-fork.ts register-xstocks || true
npx tsx src/lite-fork.ts register-lite-strategy --symbol TSLAX || true
npx tsx src/lite-fork.ts register-lite-strategy --symbol GOOGLX || true
# AAPLx needs no lite_strategy of its own — it's only ever used as ordinary margin
# COLLATERAL in the cross-asset Farm carry trade (deposit AAPLx, borrow USDC/SOL directly);
# lite_strategy is keyed by the YIELD asset (USDC/SOL, registered below), not the stock.
# One-Click/Farm's cross-asset carry trade calls `lite_supply` with the underlying_mint set to
# the YIELD asset (USDC/SOL) being deposited into Kamino, not the stock — so `lite_strategy`
# must be registered per YIELD asset here, not per stock. Registering only TSLAX/GOOGLX above
# (the old same-asset `lite_open` design's key) leaves `lite_supply` failing with
# AccountNotInitialized on "lite_strategy" for every Farm deploy on a fresh fork.
npx tsx src/lite-fork.ts register-lite-strategy --symbol USDC || true
npx tsx src/lite-fork.ts register-lite-strategy --symbol SOL || true
echo "==> Bootstrap done (SOL/USDC/TSLAx/GOOGLx/AAPLx/ANTHROPIC/OPENAI + lite strategies)."
