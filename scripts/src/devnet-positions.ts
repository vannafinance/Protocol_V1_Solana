import * as anchor from "@coral-xyz/anchor";
import { TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { AccountMeta, PublicKey } from "@solana/web3.js";
import { ata } from "./devnet-cli";
import { AssetKey, ASSET_DECIMALS, ASSET_MINTS, tokenProgramFor } from "./devnet-env";
import { CTOKEN_DECIMALS, KAMINO_RECEIPTS, KaminoReceipt, ReceiptKey } from "./kamino";
import { assetConfigPda, debtPositionPda, reservePda } from "./pda";

/** Sentinel for an empty slot in `MarginAccount.collateral_asset_indexes`/`debt_asset_indexes`. */
export const EMPTY_ASSET_INDEX = 65535;

/** A plain asset or a Kamino cToken. */
export type PositionKey = AssetKey | ReceiptKey;

export interface AssetIndexInfo {
  key: PositionKey;
  /** Asset whose Pyth price values this position (a cToken's underlying). */
  priceKey: AssetKey;
  index: number;
  mint: PublicKey;
  assetConfig: PublicKey;
  tokenProgram: PublicKey;
  /** Decimals of `mint` itself (6 for every klend cToken). */
  decimals: number;
  /** Set for a Kamino cToken: its reserve is the asset's price-source account. */
  receipt: KaminoReceipt | null;
}

function positionAssets(): Omit<AssetIndexInfo, "index" | "assetConfig">[] {
  const plain = (Object.keys(ASSET_MINTS) as AssetKey[]).map((key) => ({
    key,
    priceKey: key,
    mint: ASSET_MINTS[key],
    tokenProgram: tokenProgramFor(key),
    decimals: ASSET_DECIMALS[key],
    receipt: null,
  }));
  const receipts = (Object.keys(KAMINO_RECEIPTS) as ReceiptKey[]).map((key) => {
    const receipt = KAMINO_RECEIPTS[key];
    return {
      key,
      priceKey: receipt.underlying,
      mint: receipt.collateralMint,
      tokenProgram: TOKEN_PROGRAM_ID,
      decimals: CTOKEN_DECIMALS,
      receipt,
    };
  });
  return [...plain, ...receipts];
}

/**
 * Maps every registered asset (plain and Kamino cToken) by key. Uses `fetchNullable` because not
 * every fork registers every asset; a hard `fetch` would break every risk-checked command.
 */
export async function getAssetIndexMap(program: anchor.Program): Promise<Partial<Record<PositionKey, AssetIndexInfo>>> {
  const entries = await Promise.all(
    positionAssets().map(async (asset) => {
      const [assetConfig] = assetConfigPda(asset.mint);
      const account = await (
        program.account as Record<string, { fetchNullable(a: PublicKey): Promise<{ assetIndex: number } | null> }>
      ).assetConfig.fetchNullable(assetConfig);
      if (!account) return null;
      return [asset.key, { ...asset, index: account.assetIndex, assetConfig }] as const;
    }),
  );
  return Object.fromEntries(entries.filter((e): e is NonNullable<typeof e> => e !== null));
}

interface MarginAccountData {
  collateralAssetIndexes: number[];
  debtAssetIndexes: number[];
}

/**
 * Builds the health-check `remaining_accounts`: every active position except the named ones, in
 * `risk_engine.rs::scan_and_validate_positions` order. Collateral groups are
 * `[asset_config, margin_vault, price]`, plus the Kamino reserve for a cToken; debt groups are
 * `[asset_config, reserve, debt_position, price]`. `priceAccounts` must hold a fresh price for
 * every asset (a cToken uses its underlying's).
 */
export async function buildRemainingAccounts(
  program: anchor.Program,
  margin: PublicKey,
  priceAccounts: Record<AssetKey, PublicKey>,
  opts: { excludeCollateral?: PositionKey | PositionKey[]; excludeDebt?: AssetKey } = {},
): Promise<AccountMeta[]> {
  const marginAccount = (await (
    program.account as Record<string, { fetch(a: PublicKey): Promise<MarginAccountData> }>
  ).marginAccount.fetch(margin)) as MarginAccountData;
  const indexMap = await getAssetIndexMap(program);
  const byIndex = new Map<number, AssetIndexInfo>();
  for (const info of Object.values(indexMap)) byIndex.set(info.index, info);
  const excluded = ([] as PositionKey[]).concat(opts.excludeCollateral ?? []);
  const meta = (pubkey: PublicKey) => ({ pubkey, isWritable: false, isSigner: false });

  const metas: AccountMeta[] = [];
  for (const idx of marginAccount.collateralAssetIndexes) {
    if (idx === EMPTY_ASSET_INDEX) continue;
    const info = byIndex.get(idx);
    if (!info || excluded.includes(info.key)) continue;
    metas.push(meta(info.assetConfig), meta(ata(margin, info.mint, info.tokenProgram)), meta(priceAccounts[info.priceKey]));
    if (info.receipt) metas.push(meta(info.receipt.reserve));
  }

  for (const idx of marginAccount.debtAssetIndexes) {
    if (idx === EMPTY_ASSET_INDEX) continue;
    const info = byIndex.get(idx);
    if (!info || info.key === opts.excludeDebt) continue;
    const [reserve] = reservePda(info.mint);
    const [debtPosition] = debtPositionPda(margin, reserve);
    metas.push(meta(info.assetConfig), meta(reserve), meta(debtPosition), meta(priceAccounts[info.priceKey]));
  }
  return metas;
}
