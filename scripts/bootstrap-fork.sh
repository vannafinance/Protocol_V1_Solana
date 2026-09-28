#!/usr/bin/env bash
# Fresh Surfpool bootstrap for Vanna. Prereq: surfpool running on 127.0.0.1:8899 with the program
# auto-deployed.
#   Lending pools (Earn + borrow): USDC, USDT, SOL
#   Margin collateral only:        JitoSOL, JupSOL, JupUSD, NVDAx, TSLAx (+ Kamino cUSDC / cSOL)
#   Integrations:                  Kamino (main market), Jupiter
set -euo pipefail
export DEVNET_RPC_URL="${DEVNET_RPC_URL:-http://127.0.0.1:8899}"
cd "$(dirname "$0")"
echo "==> RPC $DEVNET_RPC_URL"
npx tsx src/devnet.ts initialize-protocol || true
for asset in usdc usdt wsol; do
  npx tsx src/devnet.ts register-asset --asset "$asset" || true
  npx tsx src/devnet.ts initialize-reserve --asset "$asset" || true
done
# Collateral-only assets. register-asset passes each asset's oracle (scripts/src/devnet-env.ts
# ASSET_ORACLES: Kamino Scope first, Pyth fallback) and refreshes it on the fork first, because
# the program reads the oracle accounts to check them.
for asset in jitosol jupsol jupusd nvdax tslax; do
  npx tsx src/devnet.ts register-asset --asset "$asset" || true
done
# Kamino: whitelist klend for `margin_execute`, then register each cToken as collateral priced
# as its underlying (registered above) through its Kamino reserve.
npx tsx src/integrations-fork.ts register-kamino || true
for symbol in USDC SOL; do
  npx tsx src/integrations-fork.ts register-receipt --symbol "$symbol" || true
done
# Jupiter: whitelist the aggregator for `margin_execute` swaps.
npx tsx src/integrations-fork.ts register-jupiter || true
echo "==> Bootstrap done: pools USDC/USDT/SOL; collateral JitoSOL/JupSOL/JupUSD/NVDAx/TSLAx; Kamino + Jupiter."
