import * as anchor from "@coral-xyz/anchor";
import { TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { Connection, Keypair, PublicKey } from "@solana/web3.js";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

/**
 * Shared environment for Protocol CLI scripts. The only supported target is a local
 * Surfpool mainnet fork (lazy-clones real mainnet accounts). Despite the historical
 * `DEVNET_RPC_URL` env name, the default is always the local fork — never public Devnet.
 */

/** Prefer DEVNET_RPC_URL for backward-compat with existing docs/scripts; FORK_RPC_URL also works. */
export const DEVNET_RPC_URL =
  process.env.DEVNET_RPC_URL ?? process.env.FORK_RPC_URL ?? "http://127.0.0.1:8899";

export type AssetKey = "usdc" | "usdt" | "wsol" | "jitosol" | "jupsol" | "jupusd" | "nvdax" | "tslax";

/** The three lending pools (Earn + borrow). */
export const POOL_ASSETS: readonly AssetKey[] = ["usdc", "usdt", "wsol"];
/** Margin collateral with no pool: deposit and use in health, never lent or borrowed. */
export const COLLATERAL_ONLY_ASSETS: readonly AssetKey[] = ["jitosol", "jupsol", "jupusd", "nvdax", "tslax"];

/** Real mainnet mints (cloned onto the Surfpool fork by address). */
export const ASSET_MINTS: Record<AssetKey, PublicKey> = {
  usdc: new PublicKey("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"),
  usdt: new PublicKey("Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB"),
  wsol: new PublicKey("So11111111111111111111111111111111111111112"),
  jitosol: new PublicKey("J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn"),
  jupsol: new PublicKey("jupSoLaHXQiZZTSfEWMTRRgpnyFm8f6sZdosWBjx93v"),
  jupusd: new PublicKey("JuprjznTrTSp2UFa3ZBUFgwdAmtZCq4MQCwysN55USD"),
  nvdax: new PublicKey("Xsc9qvGR1efVDFGLrVsmkzv3qi45LTBjeUKSPmx9qEh"),
  tslax: new PublicKey("XsDoVfqeBukxuZHWhdvWHBhgEHjGNst4MLodqsJHzoB"),
};
export const USDC_MINT = ASSET_MINTS.usdc;
export const WSOL_MINT = ASSET_MINTS.wsol;

export const ASSET_DECIMALS: Record<AssetKey, number> = {
  usdc: 6,
  usdt: 6,
  wsol: 9,
  jitosol: 9,
  jupsol: 9,
  jupusd: 6,
  nvdax: 8,
  tslax: 8,
};

/** xStocks are Token-2022 (Scaled UI Amount extension); everything else is classic SPL. */
export const ASSET_TOKEN_PROGRAM: Record<AssetKey, PublicKey> = {
  usdc: TOKEN_PROGRAM_ID,
  usdt: TOKEN_PROGRAM_ID,
  wsol: TOKEN_PROGRAM_ID,
  jitosol: TOKEN_PROGRAM_ID,
  jupsol: TOKEN_PROGRAM_ID,
  jupusd: TOKEN_PROGRAM_ID,
  nvdax: TOKEN_2022_PROGRAM_ID,
  tslax: TOKEN_2022_PROGRAM_ID,
};

/** Pyth feed ids (hex, same on every cluster) of the feeds the protocol reads, each at its
 * shard-0 push account, which Pyth keeps updated (55 s heartbeat / 0.5% deviation). */
export const PYTH_FEED_IDS = {
  usdc: "eaa020c61cc479712813461ce153894a96a6c00b21ed0cfc2798d1f9a9e9c94a", // Crypto.USDC/USD
  usdt: "2b89b9dc8fdf9f34709a5b106b472f0f39bb6ca9ce04b0fd7f2e971688e2e53b", // Crypto.USDT/USD
  wsol: "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d", // Crypto.SOL/USD
  jupsolRate: "f8d8d6b6c866c8b2624fb5b679ae846738725e5fc887fa8e927c8d8645018a2b", // Crypto.JUPSOL/SOL.RR
  jupusd: "8ed858a2214e892c9371694fb6c8a9037b6ed4052c4edf209f8cb988484e81d9", // Crypto.JUPUSD/USD
} as const;
export type PythFeedKey = keyof typeof PYTH_FEED_IDS;

export const PYTH_SHARD_ID = 0;
export const PYTH_RECEIVER_PROGRAM_ID = new PublicKey("rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ");
const PYTH_PUSH_ORACLE_PROGRAM_ID = new PublicKey("pythWSnswVUd12oZpeFP8e9CVaEqJg25g1Vtc2biRsT");

/** Pyth's canonical price-feed account for a feed (shard 0): the only Pyth account an asset may pin. */
export function pythFeedAccount(feedHex: string): PublicKey {
  const seedShard = Buffer.alloc(2);
  seedShard.writeUInt16LE(PYTH_SHARD_ID);
  return PublicKey.findProgramAddressSync([seedShard, Buffer.from(feedHex, "hex")], PYTH_PUSH_ORACLE_PROGRAM_ID)[0];
}

/** Kamino's Scope `OraclePrices` account, which Kamino's keepers refresh every ~40 s. */
export const SCOPE_PRICES = new PublicKey("3t4JZcueEzTbVP6kLxXrL3VpWx45jDer4eqysweBchNH");
export const SCOPE_PROGRAM_ID = new PublicKey("HFn8GnPADiny6XqUoWE8uRPPxb29ikn4yTuPa9MF2fWJ");
const UNUSED = 65535;
const chain = (...entries: number[]): number[] => [...entries, UNUSED, UNUSED, UNUSED, UNUSED].slice(0, 4);
const EMPTY_CHAIN = chain();

/** How one asset is priced (the program's `OracleConfig`, `oracle/mod.rs`): a Scope chain first,
 * a Pyth feed (× an optional second feed) as the only source or the fallback. */
export interface AssetOracle {
  scope?: { chain: number[]; twap: number[] };
  pyth?: PythFeedKey;
  pythFactor?: PythFeedKey;
  maxAgeSecs: number;
  maxTwapDivergenceBps: number;
  maxConfidenceBps: number;
}

/**
 * Each asset's oracle: Kamino's Scope entries (Chainlink and Pyth Lazer cross-checked, stablecoins
 * capped at $1, liquid-staked SOL as stake rate × SOL/USD) with Kamino's own max ages and TWAP
 * limits; Pyth as the fallback where Pyth keeps the feed updated; JupUSD on Pyth alone. The
 * xStocks have no maintained Pyth account, and Scope's Chainlink price already applies their
 * Scaled UI multiplier (price per raw token).
 */
export const ASSET_ORACLES: Record<AssetKey, AssetOracle> = {
  usdc: { scope: { chain: chain(13), twap: chain(456) }, pyth: "usdc", maxAgeSecs: 180, maxTwapDivergenceBps: 300, maxConfidenceBps: 200 },
  usdt: { scope: { chain: chain(16), twap: chain(457) }, pyth: "usdt", maxAgeSecs: 300, maxTwapDivergenceBps: 300, maxConfidenceBps: 200 },
  wsol: { scope: { chain: chain(3), twap: chain(455) }, pyth: "wsol", maxAgeSecs: 120, maxTwapDivergenceBps: 1000, maxConfidenceBps: 200 },
  jitosol: { scope: { chain: chain(210, 3), twap: chain(210, 455) }, maxAgeSecs: 120, maxTwapDivergenceBps: 1000, maxConfidenceBps: 200 },
  jupsol: {
    scope: { chain: chain(224, 3), twap: chain(224, 455) },
    pyth: "jupsolRate",
    pythFactor: "wsol",
    maxAgeSecs: 120,
    maxTwapDivergenceBps: 1000,
    maxConfidenceBps: 200,
  },
  jupusd: { pyth: "jupusd", maxAgeSecs: 180, maxTwapDivergenceBps: 300, maxConfidenceBps: 200 },
  nvdax: { scope: { chain: chain(332), twap: chain(269) }, maxAgeSecs: 300, maxTwapDivergenceBps: 500, maxConfidenceBps: 200 },
  tslax: { scope: { chain: chain(338), twap: chain(273) }, maxAgeSecs: 300, maxTwapDivergenceBps: 500, maxConfidenceBps: 200 },
};

/** A Kamino cToken's exchange-rate source: priced as its underlying × the reserve's rate. */
export interface KlendRate {
  reserve: PublicKey;
  program: PublicKey;
}

/** The `OracleConfig` instruction argument for `asset` (or a cToken of it, with `klend`). */
export function oracleConfigArg(asset: AssetKey, klend?: KlendRate) {
  const o = ASSET_ORACLES[asset];
  return {
    scopePrices: o.scope ? SCOPE_PRICES : PublicKey.default,
    scopeChain: o.scope?.chain ?? EMPTY_CHAIN,
    scopeTwapChain: o.scope?.twap ?? EMPTY_CHAIN,
    pythPrice: o.pyth ? pythFeedAccount(PYTH_FEED_IDS[o.pyth]) : PublicKey.default,
    pythFactor: o.pythFactor ? pythFeedAccount(PYTH_FEED_IDS[o.pythFactor]) : PublicKey.default,
    klendReserve: klend?.reserve ?? PublicKey.default,
    klendProgram: klend?.program ?? PublicKey.default,
    maxAgeSecs: o.maxAgeSecs,
    maxTwapDivergenceBps: o.maxTwapDivergenceBps,
    maxConfidenceBps: o.maxConfidenceBps,
  };
}

/** Every account `asset`'s price reads (Scope, Pyth, Pyth factor), in no particular order. */
export function oracleAccountsFor(asset: AssetKey): PublicKey[] {
  const o = ASSET_ORACLES[asset];
  return [
    ...(o.scope ? [SCOPE_PRICES] : []),
    ...(o.pyth ? [pythFeedAccount(PYTH_FEED_IDS[o.pyth])] : []),
    ...(o.pythFactor ? [pythFeedAccount(PYTH_FEED_IDS[o.pythFactor])] : []),
  ];
}

/** The Pyth feeds `assets` read. */
export function pythFeedsFor(assets: AssetKey[]): PythFeedKey[] {
  const feeds = new Set<PythFeedKey>();
  for (const asset of assets) {
    const o = ASSET_ORACLES[asset];
    if (o.pyth) feeds.add(o.pyth);
    if (o.pythFactor) feeds.add(o.pythFactor);
  }
  return [...feeds];
}

const ASSET_ALIASES: Record<string, AssetKey> = { sol: "wsol" };

export function assetKeyFromString(value: string): AssetKey {
  const v = value.toLowerCase();
  const key = (ASSET_ALIASES[v] ?? v) as AssetKey;
  if (key in ASSET_MINTS) return key;
  throw new Error(`unknown asset "${value}" — expected ${Object.keys(ASSET_MINTS).join("|")}`);
}

export function tokenProgramFor(asset: AssetKey): PublicKey {
  return ASSET_TOKEN_PROGRAM[asset];
}

function loadIdl(): anchor.Idl {
  const idlPath = path.resolve(__dirname, "..", "..", "target", "idl", "vanna_lending.json");
  return JSON.parse(fs.readFileSync(idlPath, "utf8")) as anchor.Idl;
}

export const IDL = loadIdl();

export function loadKeypair(explicitPath?: string): Keypair {
  const walletPath =
    explicitPath ?? process.env.ANCHOR_WALLET ?? path.join(os.homedir(), ".config", "solana", "id.json");
  const secret = JSON.parse(fs.readFileSync(walletPath, "utf8"));
  return Keypair.fromSecretKey(Uint8Array.from(secret));
}

export function devnetConnection(): Connection {
  return new Connection(DEVNET_RPC_URL, "confirmed");
}

export function programAs(connection: Connection, wallet: Keypair): anchor.Program {
  const anchorWallet = new anchor.Wallet(wallet);
  const provider = new anchor.AnchorProvider(connection, anchorWallet, { commitment: "confirmed" });
  return new anchor.Program(IDL, provider);
}

export function log(step: string, detail?: string): void {
  console.log(detail ? `✔ ${step} — ${detail}` : `✔ ${step}`);
}
