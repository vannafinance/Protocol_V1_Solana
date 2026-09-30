#!/usr/bin/env bash
set -euo pipefail
export DEVNET_RPC_URL="${DEVNET_RPC_URL:-http://127.0.0.1:8899}"
cd "$(dirname "$0")"
echo "==> RPC $DEVNET_RPC_URL"
npx tsx src/deploy-fork.ts
npx tsx src/devnet.ts initialize-protocol || true
for asset in usdc usdt wsol; do
  npx tsx src/devnet.ts register-asset --asset "$asset" || true
  npx tsx src/devnet.ts initialize-reserve --asset "$asset" || true
done
for asset in jitosol jupsol jupusd nvdax tslax; do
  npx tsx src/devnet.ts register-asset --asset "$asset" || true
done
npx tsx src/integrations-fork.ts register-kamino || true
for symbol in USDC SOL; do
  npx tsx src/integrations-fork.ts register-receipt --symbol "$symbol" || true
done
npx tsx src/integrations-fork.ts register-jupiter || true
for market in eth btc sol; do
  npx tsx src/integrations-fork.ts register-gmtrade --market "$market" --max-leverage 5 || true
done
echo "==> Bootstrap done: pools USDC/USDT/SOL; collateral JitoSOL/JupSOL/JupUSD/NVDAx/TSLAx; Kamino + Jupiter + GMTrade ETH/BTC/SOL."
