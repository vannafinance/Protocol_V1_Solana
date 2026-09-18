#!/usr/bin/env bash
# Fresh Surfpool bootstrap for Vanna (SOL + USDC + TSLAx + GOOGLx + Kamino lite).
# Prereq: surfpool running on 127.0.0.1:8899 with the program auto-deployed.
set -euo pipefail
export DEVNET_RPC_URL="${DEVNET_RPC_URL:-http://127.0.0.1:8899}"
cd "$(dirname "$0")"
echo "==> RPC $DEVNET_RPC_URL"
npx tsx src/devnet.ts initialize-protocol || true
npx tsx src/devnet.ts register-asset --asset wsol || true
npx tsx src/devnet.ts initialize-reserve --asset wsol || true
npx tsx src/devnet.ts register-asset --asset usdc || true
npx tsx src/devnet.ts initialize-reserve --asset usdc || true
npx tsx src/lite-fork.ts register-xstocks || true
npx tsx src/lite-fork.ts register-lite-strategy --symbol TSLAX || true
npx tsx src/lite-fork.ts register-lite-strategy --symbol GOOGLX || true
echo "==> Bootstrap done (SOL/USDC/TSLAx/GOOGLx + lite strategies)."
