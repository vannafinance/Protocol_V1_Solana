/**
 * The program's oracle facade (`programs/vanna_lending/src/oracle/`) mirrored for display, and the
 * fork helper that keeps the accounts it reads fresh.
 *
 * `readPrice` makes the same choice the program does: the asset's Scope chain while it is fresh,
 * otherwise its Pyth feed (× factor) if that is newer; so a displayed health factor matches the
 * one the program computes from the same accounts.
 */
import * as anchor from "@coral-xyz/anchor";
import { Connection, PublicKey, SYSVAR_CLOCK_PUBKEY } from "@solana/web3.js";
import {
  ASSET_ORACLES,
  AssetKey,
  PYTH_FEED_IDS,
  pythFeedAccount,
  pythFeedsFor,
  SCOPE_PRICES,
  SCOPE_PROGRAM_ID,
} from "./devnet-env";
import { callForkCheatcode, EXPONENT_OFFSET, PRICE_OFFSET, PUBLISH_TIME_OFFSET, refreshPrice } from "./devnet-pyth";
import { normalizeTokenValue } from "./devnet-math";

const MAINNET_RPC_URL = process.env.MAINNET_RPC_URL ?? "https://api.mainnet-beta.solana.com";
const UNUSED = 65535;

/** `OraclePrices`: discriminator (8) | oracle_mappings (32) | 512 × DatedPrice (56 bytes:
 * value u64, exp u64, last_updated_slot u64, unix_timestamp u64, generic_data [u8; 24]). */
function scopeEntryOffset(index: number): number {
  return 40 + 56 * index;
}

export interface OraclePrice {
  /** USD per whole token = value × 10^exponent. */
  value: bigint;
  exponent: number;
  timestamp: number;
  source: "scope" | "pyth";
  fresh: boolean;
}

/** The cluster's clock (`Clock::unix_timestamp`), which the program measures price age against. */
export async function chainTime(conn: Connection): Promise<number> {
  const info = await conn.getAccountInfo(SYSVAR_CLOCK_PUBKEY);
  if (!info) throw new Error("clock sysvar not found");
  return Number(info.data.readBigInt64LE(32));
}

function readScopeChain(data: Buffer, chain: number[]): { value: bigint; exponent: number; timestamp: number } | null {
  let value = 1n;
  let exponent = 0;
  let timestamp = Number.MAX_SAFE_INTEGER;
  for (const entry of chain.filter((e) => e !== UNUSED)) {
    const at = scopeEntryOffset(entry);
    const v = data.readBigUInt64LE(at);
    if (v === 0n) return null; // unusable, like the program's zero entry
    value *= v;
    exponent -= Number(data.readBigUInt64LE(at + 8));
    timestamp = Math.min(timestamp, Number(data.readBigUInt64LE(at + 24)));
  }
  return { value, exponent, timestamp };
}

async function readPyth(conn: Connection, feed: keyof typeof PYTH_FEED_IDS) {
  const info = await conn.getAccountInfo(pythFeedAccount(PYTH_FEED_IDS[feed]));
  if (!info) throw new Error(`Pyth ${feed} feed account missing — refresh oracles first`);
  return {
    value: info.data.readBigInt64LE(PRICE_OFFSET),
    exponent: info.data.readInt32LE(EXPONENT_OFFSET),
    timestamp: Number(info.data.readBigInt64LE(PUBLISH_TIME_OFFSET)),
  };
}

/** `asset`'s price as the program would read it now. */
export async function readPrice(conn: Connection, asset: AssetKey): Promise<OraclePrice> {
  const o = ASSET_ORACLES[asset];
  const now = await chainTime(conn);
  const isFresh = (t: number) => now - t <= o.maxAgeSecs;
  let quote: Omit<OraclePrice, "fresh"> | null = null;
  if (o.scope) {
    const info = await conn.getAccountInfo(SCOPE_PRICES);
    if (!info) throw new Error("Scope prices account missing — refresh oracles first");
    const chain = readScopeChain(info.data, o.scope.chain);
    if (chain) quote = { ...chain, source: "scope" };
  }
  if (o.pyth && !(quote && isFresh(quote.timestamp))) {
    const base = await readPyth(conn, o.pyth);
    let fallback = { ...base, source: "pyth" as const };
    if (o.pythFactor) {
      const factor = await readPyth(conn, o.pythFactor);
      fallback = {
        value: base.value * factor.value,
        exponent: base.exponent + factor.exponent,
        timestamp: Math.min(base.timestamp, factor.timestamp),
        source: "pyth",
      };
    }
    if (!quote || fallback.timestamp > quote.timestamp) quote = fallback;
  }
  if (!quote) throw new Error(`${asset}: no usable price`);
  return { ...quote, fresh: isFresh(quote.timestamp) };
}

/** USD value (nano-USD) of `amount` raw units at `price`: the program's `OraclePrice::value_of`. */
export function valueOf(price: OraclePrice, amount: bigint, decimals: number, roundUp: boolean): bigint {
  return normalizeTokenValue(amount, price.value, price.exponent, decimals, roundUp);
}

/**
 * Makes every price `assets` read fresh on the local Surfpool fork, before an instruction that
 * checks them:
 * - Pyth feeds: a real Hermes update (with PYTH_API_KEY), else fabricated on the fork.
 * - Scope: Kamino's live `OraclePrices` account copied from mainnet onto the fork, with the entries
 *   these assets read restamped to the fork's clock (a fork's clock need not match mainnet's).
 * The fork cheatcodes make this local-only; on a real cluster the keepers keep both fresh.
 */
export async function refreshOraclesOnFork(conn: Connection, wallet: anchor.Wallet, assets: AssetKey[]): Promise<void> {
  for (const feed of pythFeedsFor(assets)) await refreshPrice(conn, wallet, feed);

  const entries = new Set<number>();
  for (const asset of assets) {
    const scope = ASSET_ORACLES[asset].scope;
    if (scope) [...scope.chain, ...scope.twap].filter((e) => e !== UNUSED).forEach((e) => entries.add(e));
  }
  if (entries.size === 0) return;
  const live = await new Connection(MAINNET_RPC_URL, "confirmed").getAccountInfo(SCOPE_PRICES);
  if (!live?.owner.equals(SCOPE_PROGRAM_ID)) throw new Error(`could not read Scope prices from ${MAINNET_RPC_URL}`);
  const data = Buffer.from(live.data);
  const now = BigInt(await chainTime(conn));
  for (const entry of entries) data.writeBigUInt64LE(now, scopeEntryOffset(entry) + 24);
  await callForkCheatcode(conn.rpcEndpoint, "surfnet_setAccount", [
    SCOPE_PRICES.toBase58(),
    { lamports: live.lamports, data: data.toString("hex"), owner: SCOPE_PROGRAM_ID.toBase58(), executable: false, rentEpoch: 0 },
  ]);
  console.log(`✔ Scope prices copied from mainnet (${entries.size} entries restamped to the fork clock)`);
}

/** Every oracle account the given assets read, each once. */
export function oracleMetas(accounts: PublicKey[]): { pubkey: PublicKey; isWritable: boolean; isSigner: boolean }[] {
  const seen = new Set<string>();
  return accounts
    .filter((key) => !seen.has(key.toBase58()) && seen.add(key.toBase58()))
    .map((pubkey) => ({ pubkey, isWritable: false, isSigner: false }));
}
