import * as anchor from "@coral-xyz/anchor";
import { ASSOCIATED_TOKEN_PROGRAM_ID, TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { AccountMeta, Connection, PublicKey, SystemProgram } from "@solana/web3.js";
import { ata } from "./devnet-cli";
import { ASSET_MINTS, oracleAccountsFor, PYTH_FEED_IDS, pythFeedAccount, PythFeedKey, SCOPE_PRICES, SCOPE_PROGRAM_ID } from "./devnet-env";
import { callForkCheatcode, EXPONENT_OFFSET, PRICE_OFFSET, refreshPrice } from "./devnet-pyth";
import { chainTime } from "./oracle";
import { venueAccountPda, marketBookPda, VALIDATOR } from "./pda";
import { agentSegment } from "./agents";

export const GMTRADE = new PublicKey("Gmso1uvJnLbawvw7yezdfCDcPydwW2s2iqG3w6MDucLo");
export const GMTRADE_STORE = new PublicKey("CTDLvGGXnoxvqLyTpGzdGLg9pD6JexKxKXSV8tqqo8bN");
export const COLLATERAL = ASSET_MINTS.usdc;
const UNUSED = 65535;
const chain = (...entries: number[]): number[] => [...entries, UNUSED, UNUSED, UNUSED, UNUSED].slice(0, 4);

export type MarketKey = "eth" | "btc" | "sol";

export interface GmMarketConfig {
  name: string;
  market: PublicKey;
  marketToken: PublicKey;
  indexMint: PublicKey;
  indexDecimals: number;
  scope?: { prices: PublicKey; chain: number[]; twap: number[] };
  pyth: PythFeedKey;
  maxAgeSecs: number;
  maxTwapDivergenceBps: number;
  maxConfidenceBps: number;
}

export const GM_MARKETS: Record<MarketKey, GmMarketConfig> = {
  eth: {
    name: "ETH/USD[USDC-USDC]",
    market: new PublicKey("6EnZdBzJsGznoh857PuhbrnrzWYGtZe6xMZiQjAPyFGT"),
    marketToken: new PublicKey("DAY6Qr1FKgJQFvjJAhFUZUWHzx8UbbbkRmt6G6AYswWG"),
    indexMint: new PublicKey("EthK4kKnQQUd1Ae1w7sdiMAUaJwq2RMwr7AtscXEdEsF"),
    indexDecimals: 8,
    scope: { prices: new PublicKey("3NJYftD5sjVfxSnUdZ1wVML8f3aC6mp1CXCL6L7TnU8C"), chain: chain(246), twap: chain(53) },
    pyth: "eth",
    maxAgeSecs: 120,
    maxTwapDivergenceBps: 1000,
    maxConfidenceBps: 200,
  },
  btc: {
    name: "BTC/USD[USDC-USDC]",
    market: new PublicKey("4tM9cPqNpEYmstNdJMCc6rwdq42939w1SRFYoqMsqPQF"),
    marketToken: new PublicKey("Dqq58gS1TgRMDouUbdvhhzc51XXTNHG921WLxH9X2eB8"),
    indexMint: new PublicKey("BtcTQYRj7HRRk7MwnWiTFj8rWqN2ALt2QYig4cSWqTbv"),
    indexDecimals: 8,
    pyth: "btc",
    maxAgeSecs: 120,
    maxTwapDivergenceBps: 1000,
    maxConfidenceBps: 200,
  },
  sol: {
    name: "SOL/USD[USDC-USDC]",
    market: new PublicKey("CJg17Dn4xgUyEW3gKSSyteNw7LhP1o9pzm9eLtvuNjkQ"),
    marketToken: new PublicKey("6UU9sF5fryafHDYPcmVcV7ucfnYs6iMVcvb8p7SBQgTc"),
    indexMint: new PublicKey("So1Zu7vPQQxrguzUehKAyVLpjcc769zxgBuDAsxTUMH"),
    indexDecimals: 9,
    scope: { prices: SCOPE_PRICES, chain: chain(3), twap: chain(455) },
    pyth: "wsol",
    maxAgeSecs: 120,
    maxTwapDivergenceBps: 1000,
    maxConfidenceBps: 200,
  },
};

export function marketKeyFromString(value: string): MarketKey {
  const key = value.toLowerCase().replace(/-perp$/, "") as MarketKey;
  if (key in GM_MARKETS) return key;
  throw new Error(`unknown GMTrade market "${value}" — expected ${Object.keys(GM_MARKETS).join("|")}`);
}

export function marketKeyOf(market: PublicKey): MarketKey {
  const key = (Object.keys(GM_MARKETS) as MarketKey[]).find((k) => GM_MARKETS[k].market.equals(market));
  if (!key) throw new Error(`market ${market.toBase58()} is listed but not in GM_MARKETS (scripts/src/gmtrade.ts)`);
  return key;
}

export function indexOracleConfigArg(key: MarketKey) {
  const m = GM_MARKETS[key];
  return {
    scopePrices: m.scope?.prices ?? PublicKey.default,
    scopeChain: m.scope?.chain ?? chain(),
    scopeTwapChain: m.scope?.twap ?? chain(),
    pythPrice: pythFeedAccount(PYTH_FEED_IDS[m.pyth]),
    pythFactor: PublicKey.default,
    klendReserve: PublicKey.default,
    klendProgram: PublicKey.default,
    maxAgeSecs: m.maxAgeSecs,
    maxTwapDivergenceBps: m.maxTwapDivergenceBps,
    maxConfidenceBps: m.maxConfidenceBps,
  };
}

export function indexOracleAccounts(key: MarketKey): PublicKey[] {
  const m = GM_MARKETS[key];
  return [...(m.scope ? [m.scope.prices] : []), pythFeedAccount(PYTH_FEED_IDS[m.pyth])];
}

export const MARKET_BOOK = marketBookPda(GMTRADE_STORE);

export interface BookEntry {
  key: MarketKey;
  leg: number;
  market: PublicKey;
  tradingEnabled: boolean;
  maxLeverageBps: number;
}

const BOOK_MARKET_COUNT = 8 + 98;
const BOOK_MARKETS = 8 + 284;
const ENTRY_LEN = 256;

export async function readMarketBook(conn: Connection): Promise<BookEntry[]> {
  const info = await conn.getAccountInfo(MARKET_BOOK);
  if (!info) return [];
  const count = info.data[BOOK_MARKET_COUNT];
  return Array.from({ length: count }, (_, leg) => {
    const at = BOOK_MARKETS + ENTRY_LEN * leg;
    const market = new PublicKey(info.data.subarray(at, at + 32));
    return { key: marketKeyOf(market), leg, market, tradingEnabled: info.data[at + 65] !== 0, maxLeverageBps: info.data.readUInt32LE(at + 68) };
  });
}

export function entryOf(book: BookEntry[], key: MarketKey): BookEntry {
  const entry = book.find((e) => e.key === key);
  if (!entry) throw new Error(`${key} is not listed in the market book — run register-gmtrade --market ${key}`);
  return entry;
}

const enc = (s: string) => Buffer.from(s, "utf8");

export function venueAccountOf(margin: PublicKey): PublicKey {
  return venueAccountPda(margin, GMTRADE_STORE);
}

export function orderNonce(leg: number, isLong: boolean): Buffer {
  const nonce = Buffer.alloc(32);
  nonce[0] = isLong ? 1 : 2;
  nonce[1] = leg;
  return nonce;
}

export function gmUserPda(owner: PublicKey): PublicKey {
  return PublicKey.findProgramAddressSync([enc("user"), GMTRADE_STORE.toBuffer(), owner.toBuffer()], GMTRADE)[0];
}

export function gmOrderPda(owner: PublicKey, leg: number, isLong: boolean): PublicKey {
  return PublicKey.findProgramAddressSync([enc("order"), GMTRADE_STORE.toBuffer(), owner.toBuffer(), orderNonce(leg, isLong)], GMTRADE)[0];
}

export function gmPositionPda(owner: PublicKey, key: MarketKey, isLong: boolean): PublicKey {
  return PublicKey.findProgramAddressSync(
    [enc("position"), GMTRADE_STORE.toBuffer(), owner.toBuffer(), GM_MARKETS[key].marketToken.toBuffer(), COLLATERAL.toBuffer(), Buffer.from([isLong ? 1 : 2])],
    GMTRADE,
  )[0];
}

export function gmEscrow(owner: PublicKey, leg: number, isLong: boolean): PublicKey {
  return ata(gmOrderPda(owner, leg, isLong), COLLATERAL, TOKEN_PROGRAM_ID);
}

const EVENT_AUTHORITY = PublicKey.findProgramAddressSync([enc("__event_authority")], GMTRADE)[0];
const STORE_WALLET = PublicKey.findProgramAddressSync([enc("store_wallet"), GMTRADE_STORE.toBuffer()], GMTRADE)[0];

export const legBit = (leg: number) => 1n << BigInt(leg);

export function venueAccountPriceAccounts(venueAccount: PublicKey, book: BookEntry[], legs: bigint): PublicKey[] {
  const keys = [MARKET_BOOK, ...oracleAccountsFor("usdc"), ata(venueAccount, COLLATERAL, TOKEN_PROGRAM_ID)];
  for (const entry of book.filter((e) => (legs & legBit(e.leg)) !== 0n)) {
    keys.push(entry.market, ...indexOracleAccounts(entry.key));
    for (const isLong of [true, false]) {
      keys.push(gmOrderPda(venueAccount, entry.leg, isLong), gmEscrow(venueAccount, entry.leg, isLong), gmPositionPda(venueAccount, entry.key, isLong));
    }
  }
  return keys;
}

export function validatorSegment(): AccountMeta[] {
  return agentSegment(VALIDATOR, [MARKET_BOOK]);
}

export const MARKET_INCREASE = 3;
export const MARKET_DECREASE = 4;
const DISCRIMINATORS = {
  prepareUser: [190, 173, 143, 193, 139, 80, 231, 133],
  preparePosition: [178, 215, 55, 90, 137, 15, 108, 15],
  createOrderV2: [200, 157, 3, 182, 3, 164, 162, 240],
  closeOrderV2: [213, 217, 98, 100, 225, 205, 76, 184],
  closeEmptyPosition: [175, 105, 138, 38, 237, 235, 250, 59],
};

export interface OrderParams {
  kind: number;
  executionLamports: bigint;
  initialCollateralDeltaAmount: bigint;
  sizeDeltaValue: bigint;
  isLong: boolean;
  acceptablePrice: bigint | null;
}

const u64 = (v: bigint) => {
  const b = Buffer.alloc(8);
  b.writeBigUInt64LE(v);
  return b;
};
const u128 = (v: bigint) => Buffer.concat([u64(v & ((1n << 64n) - 1n)), u64(v >> 64n)]);
const option = (v: Buffer | null) => (v ? Buffer.concat([Buffer.from([1]), v]) : Buffer.from([0]));

function paramsData(p: OrderParams): Buffer {
  return Buffer.concat([
    Buffer.from([p.kind]),
    option(null),
    u64(p.executionLamports),
    Buffer.from([0]),
    u64(p.initialCollateralDeltaAmount),
    u128(p.sizeDeltaValue),
    Buffer.from([p.isLong ? 1 : 0, 1]),
    option(null),
    option(null),
    option(p.acceptablePrice === null ? null : u128(p.acceptablePrice)),
    Buffer.from([0]),
    option(null),
  ]);
}

export const prepareUserData = () => Buffer.from(DISCRIMINATORS.prepareUser);
export const preparePositionData = (p: OrderParams) => Buffer.concat([Buffer.from(DISCRIMINATORS.preparePosition), paramsData(p)]);
export const createOrderData = (p: OrderParams, leg: number) =>
  Buffer.concat([Buffer.from(DISCRIMINATORS.createOrderV2), orderNonce(leg, p.isLong), paramsData(p), option(null)]);
export const closeEmptyPositionData = () => Buffer.from(DISCRIMINATORS.closeEmptyPosition);
export function closeOrderData(reason: string): Buffer {
  const text = Buffer.from(reason, "utf8");
  const len = Buffer.alloc(4);
  len.writeUInt32LE(text.length);
  return Buffer.concat([Buffer.from(DISCRIMINATORS.closeOrderV2), len, text]);
}

const ro = (pubkey: PublicKey): AccountMeta => ({ pubkey, isSigner: false, isWritable: false });
const w = (pubkey: PublicKey): AccountMeta => ({ pubkey, isSigner: false, isWritable: true });
const unset = () => ro(GMTRADE);

export function prepareUserAccounts(venueAccount: PublicKey): AccountMeta[] {
  return [w(venueAccount), ro(GMTRADE_STORE), w(gmUserPda(venueAccount)), ro(SystemProgram.programId)];
}

export function preparePositionAccounts(venueAccount: PublicKey, key: MarketKey, isLong: boolean): AccountMeta[] {
  return [w(venueAccount), ro(GMTRADE_STORE), ro(GM_MARKETS[key].market), w(gmPositionPda(venueAccount, key, isLong)), ro(SystemProgram.programId)];
}

export function createOrderAccounts(venueAccount: PublicKey, entry: BookEntry, isLong: boolean, increase: boolean): AccountMeta[] {
  const escrow = gmEscrow(venueAccount, entry.leg, isLong);
  const onIncrease = (meta: AccountMeta) => (increase ? meta : unset());
  return [
    w(venueAccount),
    ro(venueAccount),
    ro(GMTRADE_STORE),
    w(entry.market),
    w(gmUserPda(venueAccount)),
    w(gmOrderPda(venueAccount, entry.leg, isLong)),
    w(gmPositionPda(venueAccount, entry.key, isLong)),
    onIncrease(ro(COLLATERAL)),
    ro(COLLATERAL),
    ro(COLLATERAL),
    ro(COLLATERAL),
    onIncrease(w(escrow)),
    w(escrow),
    w(escrow),
    w(escrow),
    onIncrease(w(ata(venueAccount, COLLATERAL, TOKEN_PROGRAM_ID))),
    ro(SystemProgram.programId),
    ro(TOKEN_PROGRAM_ID),
    ro(ASSOCIATED_TOKEN_PROGRAM_ID),
    unset(),
    unset(),
    unset(),
    unset(),
    ro(EVENT_AUTHORITY),
    ro(GMTRADE),
  ];
}

export function closeOrderAccounts(venueAccount: PublicKey, entry: BookEntry, isLong: boolean, increase: boolean): AccountMeta[] {
  const escrow = gmEscrow(venueAccount, entry.leg, isLong);
  const refund = ata(venueAccount, COLLATERAL, TOKEN_PROGRAM_ID);
  const initial = (meta: AccountMeta) => (increase ? meta : unset());
  const output = (meta: AccountMeta) => (increase ? unset() : meta);
  return [
    w(venueAccount),
    w(GMTRADE_STORE),
    w(STORE_WALLET),
    w(venueAccount),
    w(venueAccount),
    w(venueAccount),
    w(gmUserPda(venueAccount)),
    unset(),
    w(gmOrderPda(venueAccount, entry.leg, isLong)),
    initial(ro(COLLATERAL)),
    output(ro(COLLATERAL)),
    ro(COLLATERAL),
    ro(COLLATERAL),
    initial(w(escrow)),
    output(w(escrow)),
    w(escrow),
    w(escrow),
    initial(w(refund)),
    output(w(refund)),
    w(refund),
    w(refund),
    ro(SystemProgram.programId),
    ro(TOKEN_PROGRAM_ID),
    ro(ASSOCIATED_TOKEN_PROGRAM_ID),
    unset(),
    unset(),
    unset(),
    unset(),
    ro(EVENT_AUTHORITY),
    ro(GMTRADE),
  ];
}

export function closeEmptyPositionAccounts(venueAccount: PublicKey, key: MarketKey, isLong: boolean): AccountMeta[] {
  return [w(venueAccount), ro(GMTRADE_STORE), w(gmPositionPda(venueAccount, key, isLong))];
}

export const USD_UNIT = 10n ** 20n;

export interface GmPosition {
  sizeInTokens: bigint;
  collateralAmount: bigint;
  sizeInUsd: bigint;
  borrowingFactor: bigint;
  fundingFeeAmountPerSize: bigint;
}

const u128At = (data: Buffer, at: number) => data.readBigUInt64LE(at) + (data.readBigUInt64LE(at + 8) << 64n);

export function decodePosition(data: Buffer): GmPosition {
  const at = (i: number) => u128At(data, 8 + i);
  return { sizeInTokens: at(176), collateralAmount: at(192), sizeInUsd: at(208), borrowingFactor: at(224), fundingFeeAmountPerSize: at(240) };
}

export function encodePosition(template: Buffer, p: GmPosition): Buffer {
  const data = Buffer.from(template);
  const put = (i: number, v: bigint) => {
    data.writeBigUInt64LE(v & ((1n << 64n) - 1n), 8 + i);
    data.writeBigUInt64LE(v >> 64n, 8 + i + 8);
  };
  put(176, p.sizeInTokens);
  put(192, p.collateralAmount);
  put(208, p.sizeInUsd);
  put(224, p.borrowingFactor);
  put(240, p.fundingFeeAmountPerSize);
  return data;
}

export interface GmMarket {
  closeFeeFactor: bigint;
  borrowingFactor: [bigint, bigint];
  fundingPerSize: [bigint, bigint];
}

export function decodeMarket(data: Buffer): GmMarket {
  const pool = (index: number) => 8 + 1952 + 64 * index;
  const side = (index: number, long: boolean) => {
    const base = pool(index);
    const [l, s] = [u128At(data, base + 32), u128At(data, base + 48)];
    const pure = data[base + 16] !== 0;
    return pure ? (long ? (l + 1n) / 2n : l / 2n) : long ? l : s;
  };
  return {
    closeFeeFactor: u128At(data, 8 + 560),
    borrowingFactor: [side(8, true), side(8, false)],
    fundingPerSize: [side(9, true), side(10, true)],
  };
}

export function decodeOrder(data: Buffer): { kind: number; initialCollateralDeltaAmount: bigint; sizeDeltaValue: bigint } {
  return {
    kind: data[8 + 2096],
    initialCollateralDeltaAmount: data.readBigUInt64LE(8 + 2168),
    sizeDeltaValue: u128At(data, 8 + 2176),
  };
}

export interface PendingOrder {
  kind: number;
  initialCollateralDeltaAmount: bigint;
  sizeDeltaValue: bigint;
  escrowed: bigint;
}

export interface VenueAccountMarket {
  entry: BookEntry;
  orders: (PendingOrder | null)[];
  positions: (GmPosition | null)[];
  market: GmMarket;
}

export interface VenueAccount {
  venueAccount: PublicKey;
  lamports: number;
  idle: bigint;
  userExists: boolean;
  markets: VenueAccountMarket[];
}

const tokenAmount = (info: { data: Buffer } | null) => (info && info.data.length >= 72 ? info.data.readBigUInt64LE(64) : 0n);

export async function readVenueAccount(conn: Connection, margin: PublicKey, book: BookEntry[]): Promise<VenueAccount> {
  const venueAccount = venueAccountOf(margin);
  const [venueAccountInfo, idle, user] = await conn.getMultipleAccountsInfo([venueAccount, ata(venueAccount, COLLATERAL, TOKEN_PROGRAM_ID), gmUserPda(venueAccount)]);
  const markets: VenueAccountMarket[] = [];
  for (const entry of book) {
    const sides = [true, false];
    const keys = [
      entry.market,
      ...sides.map((long) => gmOrderPda(venueAccount, entry.leg, long)),
      ...sides.map((long) => gmEscrow(venueAccount, entry.leg, long)),
      ...sides.map((long) => gmPositionPda(venueAccount, entry.key, long)),
    ];
    const [market, orderLong, orderShort, escrowLong, escrowShort, posLong, posShort] = await conn.getMultipleAccountsInfo(keys);
    if (!market) throw new Error(`GMTrade market ${entry.market.toBase58()} not found`);
    const order = (info: typeof orderLong, escrow: typeof escrowLong) =>
      info && info.owner.equals(GMTRADE) && info.data.length > 0 ? { ...decodeOrder(info.data), escrowed: tokenAmount(escrow) } : null;
    const position = (info: typeof posLong) => (info && info.owner.equals(GMTRADE) ? decodePosition(info.data) : null);
    markets.push({
      entry,
      orders: [order(orderLong, escrowLong), order(orderShort, escrowShort)],
      positions: [position(posLong), position(posShort)],
      market: decodeMarket(market.data),
    });
  }
  return { venueAccount, lamports: venueAccountInfo?.lamports ?? 0, idle: tokenAmount(idle), userExists: !!user && user.data.length > 0, markets };
}

export function isOpen(m: VenueAccountMarket): boolean {
  return m.orders.some((o) => o !== null) || m.positions.some((p) => p !== null && (p.sizeInUsd > 0n || p.collateralAmount > 0n));
}

export function venueAccountEquityUsd(venueAccount: VenueAccount, legs: bigint, indexPriceUsd: Partial<Record<MarketKey, number>>, usdcPriceUsd: number): number {
  let equity = (Number(venueAccount.idle) / 1e6) * usdcPriceUsd;
  for (const m of venueAccount.markets.filter((m) => (legs & legBit(m.entry.leg)) !== 0n)) {
    const index = indexPriceUsd[m.entry.key];
    if (index === undefined) throw new Error(`no index price for ${m.entry.key}`);
    for (const o of m.orders) if (o) equity += (Number(o.escrowed) / 1e6) * usdcPriceUsd;
    m.positions.forEach((p, side) => {
      if (p) equity += positionEquityUsd(p, side === 0, m.market, index, GM_MARKETS[m.entry.key].indexDecimals, usdcPriceUsd);
    });
  }
  return equity;
}

export function positionEquityUsd(p: GmPosition, isLong: boolean, market: GmMarket, indexPriceUsd: number, indexDecimals: number, usdcPriceUsd = 1): number {
  if (p.sizeInUsd === 0n && p.collateralAmount === 0n) return 0;
  const size = Number(p.sizeInUsd) / 1e20;
  const value = (Number(p.sizeInTokens) / 10 ** indexDecimals) * indexPriceUsd;
  const pnl = isLong ? value - size : size - value;
  const side = isLong ? 0 : 1;
  const borrowing = (size * Number(market.borrowingFactor[side] - p.borrowingFactor)) / 1e20;
  const fundingUsdc = (Number(p.sizeInUsd) * Number(market.fundingPerSize[side] - p.fundingFeeAmountPerSize)) / 1e30 / 1e6;
  const close = (size * Number(market.closeFeeFactor)) / 1e20;
  const collateral = (Number(p.collateralAmount) / 1e6) * usdcPriceUsd;
  return Math.max(0, collateral + pnl - Math.max(0, borrowing) - Math.max(0, fundingUsdc) * usdcPriceUsd - close);
}

const MAINNET_RPC_URL = process.env.MAINNET_RPC_URL ?? "https://api.mainnet-beta.solana.com";

export async function refreshIndexScopeOnFork(conn: Connection, key: MarketKey, now: bigint): Promise<void> {
  const scope = GM_MARKETS[key].scope;
  if (!scope || scope.prices.equals(SCOPE_PRICES)) return;
  const live = await new Connection(MAINNET_RPC_URL, "confirmed").getAccountInfo(scope.prices);
  if (!live?.owner.equals(SCOPE_PROGRAM_ID)) throw new Error(`could not read ${scope.prices.toBase58()} from ${MAINNET_RPC_URL}`);
  const data = Buffer.from(live.data);
  for (const entry of [...scope.chain, ...scope.twap].filter((e) => e !== UNUSED)) {
    data.writeBigUInt64LE(now, 40 + 56 * entry + 24);
  }
  await callForkCheatcode(conn.rpcEndpoint, "surfnet_setAccount", [
    scope.prices.toBase58(),
    { lamports: live.lamports, data: data.toString("hex"), owner: SCOPE_PROGRAM_ID.toBase58(), executable: false, rentEpoch: 0 },
  ]);
}

export async function refreshMarketsOnFork(conn: Connection, wallet: anchor.Wallet): Promise<void> {
  const feeds = new Set((Object.keys(GM_MARKETS) as MarketKey[]).map((key) => GM_MARKETS[key].pyth));
  for (const feed of feeds) await refreshPrice(conn, wallet, feed);
  const now = BigInt(await chainTime(conn));
  for (const key of Object.keys(GM_MARKETS) as MarketKey[]) await refreshIndexScopeOnFork(conn, key, now);
}

const LAST_RESTART_SLOT_SYSVAR = new PublicKey("SysvarLastRestartS1ot1111111111111111111111");
const STORE_LAST_RESTARTED_SLOT_OFFSET = 8 + 4792;

export async function alignGmtradeStoreOnFork(conn: Connection): Promise<void> {
  const store = await conn.getAccountInfo(GMTRADE_STORE);
  if (!store) throw new Error("GMTrade store not found on the fork");
  const sysvar = await conn.getAccountInfo(LAST_RESTART_SLOT_SYSVAR);
  const forkRestart = sysvar && sysvar.data.length >= 8 ? sysvar.data.readBigUInt64LE(0) : 0n;
  if (store.data.readBigUInt64LE(STORE_LAST_RESTARTED_SLOT_OFFSET) === forkRestart) return;
  const data = Buffer.from(store.data);
  data.writeBigUInt64LE(forkRestart, STORE_LAST_RESTARTED_SLOT_OFFSET);
  await callForkCheatcode(conn.rpcEndpoint, "surfnet_setAccount", [
    GMTRADE_STORE.toBase58(),
    { lamports: store.lamports, data: data.toString("hex"), owner: GMTRADE.toBase58(), executable: false, rentEpoch: 0 },
  ]);
  console.log(`✔ GMTrade store aligned with the fork's last restart slot (${forkRestart})`);
}

export async function readIndexPriceUsd(conn: Connection, key: MarketKey): Promise<number> {
  const m = GM_MARKETS[key];
  if (m.scope) {
    const info = await conn.getAccountInfo(m.scope.prices);
    if (!info) throw new Error("Scope feed missing");
    const at = 40 + 56 * m.scope.chain[0];
    return Number(info.data.readBigUInt64LE(at)) / 10 ** Number(info.data.readBigUInt64LE(at + 8));
  }
  const info = await conn.getAccountInfo(pythFeedAccount(PYTH_FEED_IDS[m.pyth]));
  if (!info) throw new Error(`Pyth ${m.pyth} feed missing — refresh oracles first`);
  return Number(info.data.readBigInt64LE(PRICE_OFFSET)) * 10 ** info.data.readInt32LE(EXPONENT_OFFSET);
}

export function usdToGm(usd: string): bigint {
  const [whole, frac = ""] = usd.split(".");
  return BigInt(whole || "0") * USD_UNIT + BigInt((frac + "0".repeat(20)).slice(0, 20));
}
