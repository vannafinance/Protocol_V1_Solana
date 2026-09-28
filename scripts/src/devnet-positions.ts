import * as anchor from "@coral-xyz/anchor";
import { TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { AccountMeta, PublicKey } from "@solana/web3.js";
import { ata } from "./devnet-cli";
import { AssetKey, ASSET_DECIMALS, ASSET_MINTS, oracleAccountsFor, tokenProgramFor } from "./devnet-env";
import { oracleMetas } from "./oracle";
import { CTOKEN_DECIMALS, KAMINO_RECEIPTS, KaminoReceipt, ReceiptKey } from "./kamino";
import { assetConfigPda, debtPositionPda, reservePda } from "./pda";

/** Sentinel for an empty slot in `MarginAccount.collateral_asset_indexes`/`debt_asset_indexes`. */
export const EMPTY_ASSET_INDEX = 65535;

/** A plain asset or a Kamino cToken. */
export type PositionKey = AssetKey | ReceiptKey;

export interface AssetIndexInfo {
  key: PositionKey;
  /** Asset whose oracle prices this position (a cToken's underlying). */
  priceKey: AssetKey;
  index: number;
  mint: PublicKey;
  assetConfig: PublicKey;
  tokenProgram: PublicKey;
  /** Decimals of `mint` itself (6 for every klend cToken). */
  decimals: number;
  /** Set for a Kamino cToken (used to value it for display). */
  receipt: KaminoReceipt | null;
  /** Every account the program reads to price this position: its oracle's Scope / Pyth accounts,
   * plus a cToken's klend reserve. */
  oracleAccounts: PublicKey[];
}

function positionAssets(): Omit<AssetIndexInfo, "index" | "assetConfig">[] {
  const plain = (Object.keys(ASSET_MINTS) as AssetKey[]).map((key) => ({
    key,
    priceKey: key,
    mint: ASSET_MINTS[key],
    tokenProgram: tokenProgramFor(key),
    decimals: ASSET_DECIMALS[key],
    receipt: null,
    oracleAccounts: oracleAccountsFor(key),
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
      oracleAccounts: [...oracleAccountsFor(receipt.underlying), receipt.reserve],
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
 * Builds the health-check `remaining_accounts`: the position groups of every active position except
 * the named ones, in `risk_engine.rs::scan_positions` order (collateral `[asset_config,
 * margin_vault]`, debt `[asset_config, reserve, debt_position]`), then the oracle accounts of every
 * active position and of `opts.priced` (the instruction's own assets), each once.
 */
export async function buildRemainingAccounts(
  program: anchor.Program,
  margin: PublicKey,
  opts: { excludeCollateral?: PositionKey | PositionKey[]; excludeDebt?: AssetKey; priced?: PositionKey[] } = {},
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
  const oracles: PublicKey[] = (opts.priced ?? []).flatMap((key) => positionOracleAccounts(key));
  for (const idx of marginAccount.collateralAssetIndexes) {
    if (idx === EMPTY_ASSET_INDEX) continue;
    const info = byIndex.get(idx);
    if (!info) continue;
    oracles.push(...info.oracleAccounts);
    if (excluded.includes(info.key)) continue;
    metas.push(meta(info.assetConfig), meta(ata(margin, info.mint, info.tokenProgram)));
  }

  for (const idx of marginAccount.debtAssetIndexes) {
    if (idx === EMPTY_ASSET_INDEX) continue;
    const info = byIndex.get(idx);
    if (!info) continue;
    oracles.push(...info.oracleAccounts);
    if (info.key === opts.excludeDebt) continue;
    const [reserve] = reservePda(info.mint);
    const [debtPosition] = debtPositionPda(margin, reserve);
    metas.push(meta(info.assetConfig), meta(reserve), meta(debtPosition));
  }
  return [...metas, ...oracleMetas(oracles)];
}

/** The oracle accounts of a position key, registered or not. */
function positionOracleAccounts(key: PositionKey): PublicKey[] {
  const asset = positionAssets().find((a) => a.key === key);
  return asset ? asset.oracleAccounts : [];
}

export interface LiquidationAccounts {
  /** `remaining_accounts` for `public_liquidate`. */
  metas: AccountMeta[];
  /** The liquidator's token accounts the collateral is swept into (create them first). */
  destinations: { mint: PublicKey; tokenProgram: PublicKey; account: PublicKey }[];
}

/**
 * `public_liquidate` accounts for every position of `margin`: the position groups (every position,
 * margin vaults / reserves / debt positions writable), then per collateral `[mint, destination,
 * token_program]`, then per debt `[mint, reserve vault, source, token_program]`, then every
 * position's oracle accounts. Collateral is swept to `liquidator`'s ATAs and debts are repaid from
 * them.
 */
export async function buildLiquidationAccounts(
  program: anchor.Program,
  margin: PublicKey,
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
  const oracles: PublicKey[] = [];
  const destinations: LiquidationAccounts["destinations"] = [];
  for (const idx of marginAccount.collateralAssetIndexes) {
    if (idx === EMPTY_ASSET_INDEX) continue;
    const info = lookup(idx);
    health.push(ro(info.assetConfig), w(ata(margin, info.mint, info.tokenProgram)));
    oracles.push(...info.oracleAccounts);
    const destination = ata(liquidator, info.mint, info.tokenProgram);
    destinations.push({ mint: info.mint, tokenProgram: info.tokenProgram, account: destination });
    settlement.push(ro(info.mint), w(destination), ro(info.tokenProgram));
  }
  for (const idx of marginAccount.debtAssetIndexes) {
    if (idx === EMPTY_ASSET_INDEX) continue;
    const info = lookup(idx);
    const [reserve] = reservePda(info.mint);
    health.push(ro(info.assetConfig), w(reserve), w(debtPositionPda(margin, reserve)[0]));
    oracles.push(...info.oracleAccounts);
    settlement.push(
      ro(info.mint),
      w(ata(reserve, info.mint, info.tokenProgram)),
      w(ata(liquidator, info.mint, info.tokenProgram)),
      ro(info.tokenProgram),
    );
  }
  return { metas: [...health, ...settlement, ...oracleMetas(oracles)], destinations };
}
