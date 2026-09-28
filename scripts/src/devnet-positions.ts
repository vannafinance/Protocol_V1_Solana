import * as anchor from "@coral-xyz/anchor";
import { TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { AccountMeta, PublicKey } from "@solana/web3.js";
import { ata } from "./devnet-cli";
import { AssetKey, ASSET_DECIMALS, ASSET_MINTS, priceSourceAccountFor, tokenProgramFor } from "./devnet-env";
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
  /** Set for a Kamino cToken (used to value it for display). */
  receipt: KaminoReceipt | null;
  /** The extra account the program reads to value this asset, appended to its health group:
   * the Kamino reserve, the xStock mint, or JupSOL's SOL/USD feed. Null for plain Pyth assets. */
  priceSource: PublicKey | null;
}

function positionAssets(): Omit<AssetIndexInfo, "index" | "assetConfig">[] {
  const plain = (Object.keys(ASSET_MINTS) as AssetKey[]).map((key) => ({
    key,
    priceKey: key,
    mint: ASSET_MINTS[key],
    tokenProgram: tokenProgramFor(key),
    decimals: ASSET_DECIMALS[key],
    receipt: null,
    priceSource: priceSourceAccountFor(key),
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
      priceSource: receipt.reserve,
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
 * `[asset_config, margin_vault, price]`, plus the price-source account for a non-Pyth asset
 * (Kamino reserve, xStock mint, JupSOL's SOL/USD feed); debt groups are
 * `[asset_config, reserve, debt_position, price]`. `priceAccounts` must hold a fresh price for
 * every asset (a cToken uses its underlying's; JupSOL's SOL/USD base is wsol's).
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
    if (info.priceSource) metas.push(meta(info.priceSource));
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

export interface LiquidationAccounts {
  /** `remaining_accounts` for `public_liquidate`. */
  metas: AccountMeta[];
  /** The liquidator's token accounts the collateral is swept into (create them first). */
  destinations: { mint: PublicKey; tokenProgram: PublicKey; account: PublicKey }[];
}

/**
 * `public_liquidate` accounts for every position of `margin`: the health groups (every position,
 * margin vaults / reserves / debt positions writable), then per collateral `[mint, destination,
 * token_program]`, then per debt `[mint, reserve vault, source, token_program]`. Collateral is
 * swept to `liquidator`'s ATAs and debts are repaid from them.
 */
export async function buildLiquidationAccounts(
  program: anchor.Program,
  margin: PublicKey,
  priceAccounts: Record<AssetKey, PublicKey>,
  liquidator: PublicKey,
): Promise<LiquidationAccounts> {
  const marginAccount = (await (
    program.account as Record<string, { fetch(a: PublicKey): Promise<MarginAccountData> }>
  ).marginAccount.fetch(margin)) as MarginAccountData;
  const indexMap = await getAssetIndexMap(program);
  const byIndex = new Map<number, AssetIndexInfo>();
  for (const info of Object.values(indexMap)) byIndex.set(info.index, info);
  const ro = (pubkey: PublicKey) => ({ pubkey, isWritable: false, isSigner: false });
  const w = (pubkey: PublicKey) => ({ pubkey, isWritable: true, isSigner: false });
  const lookup = (idx: number) => {
    const info = byIndex.get(idx);
    if (!info) throw new Error(`margin position with unknown asset index ${idx}`);
    return info;
  };

  const health: AccountMeta[] = [];
  const settlement: AccountMeta[] = [];
  const destinations: LiquidationAccounts["destinations"] = [];
  for (const idx of marginAccount.collateralAssetIndexes) {
    if (idx === EMPTY_ASSET_INDEX) continue;
    const info = lookup(idx);
    health.push(ro(info.assetConfig), w(ata(margin, info.mint, info.tokenProgram)), ro(priceAccounts[info.priceKey]));
    if (info.priceSource) health.push(ro(info.priceSource));
    const destination = ata(liquidator, info.mint, info.tokenProgram);
    destinations.push({ mint: info.mint, tokenProgram: info.tokenProgram, account: destination });
    settlement.push(ro(info.mint), w(destination), ro(info.tokenProgram));
  }
  for (const idx of marginAccount.debtAssetIndexes) {
    if (idx === EMPTY_ASSET_INDEX) continue;
    const info = lookup(idx);
    const [reserve] = reservePda(info.mint);
    health.push(ro(info.assetConfig), w(reserve), w(debtPositionPda(margin, reserve)[0]), ro(priceAccounts[info.priceKey]));
    settlement.push(
      ro(info.mint),
      w(ata(reserve, info.mint, info.tokenProgram)),
      w(ata(liquidator, info.mint, info.tokenProgram)),
      ro(info.tokenProgram),
    );
  }
  return { metas: [...health, ...settlement], destinations };
}
