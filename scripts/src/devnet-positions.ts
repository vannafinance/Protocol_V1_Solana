import * as anchor from "@coral-xyz/anchor";
import { AccountMeta, PublicKey } from "@solana/web3.js";
import { assetConfigPda, debtPositionPda, marginPda, reservePda } from "./pda";
import { AssetKey, ASSET_MINTS } from "./devnet-env";
import { ata } from "./devnet-cli";

/** Sentinel for an empty slot in `MarginAccount.collateral_asset_indexes`/`debt_asset_indexes`. */
export const EMPTY_ASSET_INDEX = 65535;

export interface AssetIndexInfo {
  key: AssetKey;
  index: number;
  mint: PublicKey;
  assetConfig: PublicKey;
}

/** Fetches both registered `AssetConfig`s and maps `asset_index -> {key, mint, assetConfig}`. */
export async function getAssetIndexMap(program: anchor.Program): Promise<Record<AssetKey, AssetIndexInfo>> {
  const entries = await Promise.all(
    (Object.keys(ASSET_MINTS) as AssetKey[]).map(async (key) => {
      const mint = ASSET_MINTS[key];
      const [assetConfigAddress] = assetConfigPda(mint);
      const account = await (program.account as Record<string, { fetch(a: PublicKey): Promise<{ assetIndex: number }> }>)
        .assetConfig.fetch(assetConfigAddress);
      return [key, { key, index: account.assetIndex, mint, assetConfig: assetConfigAddress }] as const;
    }),
  );
  return Object.fromEntries(entries) as Record<AssetKey, AssetIndexInfo>;
}

interface MarginAccountData {
  collateralAssetIndexes: number[];
  debtAssetIndexes: number[];
}

/**
 * Builds the `remaining_accounts` list required by `user_borrow`, `user_withdraw_collateral`, and
 * `public_liquidate` — every currently active collateral/debt position on the margin account,
 * other than the one(s) already passed as named accounts, in the exact order
 * `validation/positions.rs::scan_and_validate_positions` expects (collateral groups first, then
 * debt groups). Since only `usdc`/`wsol` are ever registered by these scripts, "every other active
 * position" can only be the other one of the two — so callers should always have a fresh price
 * for both before calling this.
 */
export async function buildRemainingAccounts(
  program: anchor.Program,
  margin: PublicKey,
  priceAccounts: Record<AssetKey, PublicKey>,
  opts: { excludeCollateral?: AssetKey; excludeDebt?: AssetKey } = {},
): Promise<AccountMeta[]> {
  const marginAccount = (await (
    program.account as Record<string, { fetch(a: PublicKey): Promise<MarginAccountData> }>
  ).marginAccount.fetch(margin)) as MarginAccountData;
  const indexMap = await getAssetIndexMap(program);
  const byIndex = new Map<number, AssetIndexInfo>();
  for (const info of Object.values(indexMap)) byIndex.set(info.index, info);

  const metas: AccountMeta[] = [];

  for (const idx of marginAccount.collateralAssetIndexes) {
    if (idx === EMPTY_ASSET_INDEX) continue;
    const info = byIndex.get(idx);
    if (!info || info.key === opts.excludeCollateral) continue;
    metas.push(
      { pubkey: info.assetConfig, isWritable: false, isSigner: false },
      { pubkey: ata(margin, info.mint), isWritable: false, isSigner: false },
      { pubkey: priceAccounts[info.key], isWritable: false, isSigner: false },
    );
  }

  for (const idx of marginAccount.debtAssetIndexes) {
    if (idx === EMPTY_ASSET_INDEX) continue;
    const info = byIndex.get(idx);
    if (!info || info.key === opts.excludeDebt) continue;
    const [reserve] = reservePda(info.mint);
    const [debtPosition] = debtPositionPda(margin, reserve);
    metas.push(
      { pubkey: info.assetConfig, isWritable: false, isSigner: false },
      { pubkey: reserve, isWritable: false, isSigner: false },
      { pubkey: debtPosition, isWritable: false, isSigner: false },
      { pubkey: priceAccounts[info.key], isWritable: false, isSigner: false },
    );
  }

  return metas;
}

export { marginPda };
