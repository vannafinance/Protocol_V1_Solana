import * as anchor from "@coral-xyz/anchor";
import { TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { AccountMeta, PublicKey } from "@solana/web3.js";
import { ata } from "./devnet-cli";
import { AssetKey, ASSET_DECIMALS, ASSET_MINTS, oracleAccountsFor, tokenProgramFor } from "./devnet-env";
import { CTOKEN_DECIMALS, KAMINO_RECEIPTS, KaminoReceipt, ReceiptKey } from "./kamino";
import { COLLATERAL, venueAccountPriceAccounts, venueAccountOf, GMTRADE_STORE, readMarketBook } from "./gmtrade";
import { oracleSegment } from "./agents";
import { assetConfigPda, debtPositionPda, reservePda } from "./pda";

export const EMPTY_ASSET_INDEX = 65535;

export type VenueKey = "gmtrade";

export type PositionKey = AssetKey | ReceiptKey | VenueKey;

export interface AssetIndexInfo {
  key: PositionKey;
  priceKey: AssetKey;
  index: number;
  mint: PublicKey;
  assetConfig: PublicKey;
  tokenProgram: PublicKey;
  decimals: number;
  receipt: KaminoReceipt | null;
  venue: boolean;
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
    venue: false,
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
      venue: false,
      oracleAccounts: [...oracleAccountsFor(receipt.underlying), receipt.reserve],
    };
  });
  const venues = [
    {
      key: "gmtrade" as VenueKey,
      priceKey: "usdc" as AssetKey,
      mint: GMTRADE_STORE,
      tokenProgram: TOKEN_PROGRAM_ID,
      decimals: 6,
      receipt: null,
      venue: true,
      oracleAccounts: [] as PublicKey[],
    },
  ];
  return [...plain, ...receipts, ...venues];
}

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
  venueLegs: { assetIndex: number; legs: anchor.BN }[];
}

export async function fetchMargin(program: anchor.Program, margin: PublicKey): Promise<MarginAccountData> {
  return (program.account as Record<string, { fetch(a: PublicKey): Promise<MarginAccountData> }>).marginAccount.fetch(margin);
}

export function trackedLegs(margin: MarginAccountData, venueIndex: number): bigint {
  const entry = margin.venueLegs.find((v) => v.assetIndex === venueIndex && !v.legs.isZero());
  return entry ? BigInt(entry.legs.toString()) : 0n;
}

export async function venueAccountOracleAccounts(program: anchor.Program, margin: PublicKey, extraLegs = 0n): Promise<PublicKey[]> {
  const indexMap = await getAssetIndexMap(program);
  const venue = indexMap.gmtrade;
  if (!venue) throw new Error("GMTrade is not registered as a venue — run register-gmtrade");
  const legs = trackedLegs(await fetchMargin(program, margin), venue.index) | extraLegs;
  return venueAccountPriceAccounts(venueAccountOf(margin), await readMarketBook(program.provider.connection), legs);
}

export interface RemainingOptions {
  excludeCollateral?: PositionKey | PositionKey[];
  excludeDebt?: AssetKey;
  priced?: PositionKey[];
  newAssets?: PositionKey[];
  writable?: PositionKey[];
  venueLegs?: bigint;
  validator?: AccountMeta[];
}

export async function inactiveAssets(program: anchor.Program, margin: PublicKey, keys: PositionKey[]): Promise<PositionKey[]> {
  const marginAccount = await fetchMargin(program, margin);
  const indexMap = await getAssetIndexMap(program);
  return keys.filter((key) => {
    const info = Object.values(indexMap).find((i) => i.key === key);
    return !!info && !marginAccount.collateralAssetIndexes.includes(info.index);
  });
}

export async function buildRemainingAccounts(program: anchor.Program, margin: PublicKey, opts: RemainingOptions = {}): Promise<AccountMeta[]> {
  const marginAccount = await fetchMargin(program, margin);
  const indexMap = await getAssetIndexMap(program);
  const byIndex = new Map<number, AssetIndexInfo>();
  for (const info of Object.values(indexMap)) byIndex.set(info.index, info);
  const excluded = ([] as PositionKey[]).concat(opts.excludeCollateral ?? []);
  const writable = opts.writable ?? [];
  const meta = (pubkey: PublicKey, isWritable = false) => ({ pubkey, isWritable, isSigner: false });

  const metas: AccountMeta[] = [];
  const priced = opts.priced ?? [];
  const oracles: PublicKey[] = priced.flatMap((key) => positionAssets().find((a) => a.key === key)?.oracleAccounts ?? []);
  let venueHeld = priced.includes("gmtrade");
  for (const idx of marginAccount.collateralAssetIndexes) {
    if (idx === EMPTY_ASSET_INDEX) continue;
    const info = byIndex.get(idx);
    if (!info) continue;
    oracles.push(...info.oracleAccounts);
    venueHeld ||= info.venue;
    if (excluded.includes(info.key)) continue;
    metas.push(meta(info.assetConfig), meta(holderOf(info, margin), writable.includes(info.key)));
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
  for (const key of opts.newAssets ?? []) {
    const info = Object.values(indexMap).find((i) => i.key === key);
    if (!info) throw new Error(`${key} is not registered`);
    oracles.push(...info.oracleAccounts);
    metas.push(meta(info.assetConfig), meta(holderOf(info, margin)));
  }
  const venueAccount = venueHeld ? await venueAccountOracleAccounts(program, margin, opts.venueLegs ?? 0n) : [];
  return [...metas, ...oracleSegment([...oracles, ...venueAccount]), ...(opts.validator ?? [])];
}

function holderOf(info: AssetIndexInfo, margin: PublicKey): PublicKey {
  return info.venue ? venueAccountOf(margin) : ata(margin, info.mint, info.tokenProgram);
}

export interface LiquidationAccounts {
  metas: AccountMeta[];
  destinations: { mint: PublicKey; tokenProgram: PublicKey; account: PublicKey }[];
}

export async function buildLiquidationAccounts(
  program: anchor.Program,
  margin: PublicKey,
  liquidator: PublicKey,
): Promise<LiquidationAccounts> {
  const marginAccount = await fetchMargin(program, margin);
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
  const venueAccount: PublicKey[] = [];
  let idle: PublicKey | null = null;
  for (const idx of marginAccount.collateralAssetIndexes) {
    if (idx === EMPTY_ASSET_INDEX) continue;
    const info = lookup(idx);
    oracles.push(...info.oracleAccounts);
    if (info.venue) {
      health.push(ro(info.assetConfig), ro(venueAccountOf(margin)));
      idle = ata(venueAccountOf(margin), COLLATERAL, TOKEN_PROGRAM_ID);
      venueAccount.push(...(await venueAccountOracleAccounts(program, margin)));
      const destination = ata(liquidator, COLLATERAL, TOKEN_PROGRAM_ID);
      destinations.push({ mint: COLLATERAL, tokenProgram: TOKEN_PROGRAM_ID, account: destination });
      settlement.push(ro(COLLATERAL), w(destination), ro(TOKEN_PROGRAM_ID));
      continue;
    }
    health.push(ro(info.assetConfig), w(ata(margin, info.mint, info.tokenProgram)));
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
  const unique = destinations.filter((d, i) => destinations.findIndex((e) => e.account.equals(d.account)) === i);
  const segment = oracleSegment([...oracles, ...venueAccount]).map((m) => (idle && m.pubkey.equals(idle) ? { ...m, isWritable: true } : m));
  return { metas: [...health, ...settlement, ...segment], destinations: unique };
}
