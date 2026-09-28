#!/usr/bin/env bash
# Fresh Surfpool bootstrap for Vanna (SOL + USDC + TSLAx + GOOGLx + AAPLx + ANTHROPIC + OPENAI +
# Kamino and Jupiter integrations). Prereq: surfpool running on 127.0.0.1:8899 with the program auto-deployed.
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
npx tsx src/integrations-fork.ts register-xstocks || true
# Kamino: whitelist klend for `margin_execute`, then register each cToken as collateral priced
# through its Kamino reserve.
npx tsx src/integrations-fork.ts register-kamino || true
for symbol in TSLAX GOOGLX USDC SOL; do
  npx tsx src/integrations-fork.ts register-receipt --symbol "$symbol" || true
done
# Jupiter: whitelist the aggregator for `margin_execute` swaps.
npx tsx src/integrations-fork.ts register-jupiter || true
echo "==> Bootstrap done (SOL/USDC/TSLAx/GOOGLx/AAPLx/ANTHROPIC/OPENAI + Kamino and Jupiter integrations)."
