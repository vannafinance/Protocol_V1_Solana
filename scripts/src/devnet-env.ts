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

/**
 * Pyth feed ids (hex, same on every cluster), checked against Hermes. Each is the feed the program
 * stores as `price_feed_id`:
 * - JupSOL has no USD feed; its feed is the JUPSOL/SOL redemption rate, valued × SOL/USD on-chain.
 * - The xStocks use their own token feeds (Crypto.NVDAX/USD, Crypto.TSLAX/USD), which price one UI
 *   token; the program applies the mint's Scaled UI multiplier to the raw amount.
 */
export const PYTH_FEED_IDS: Record<AssetKey, string> = {
  usdc: "eaa020c61cc479712813461ce153894a96a6c00b21ed0cfc2798d1f9a9e9c94a", // Crypto.USDC/USD
  usdt: "2b89b9dc8fdf9f34709a5b106b472f0f39bb6ca9ce04b0fd7f2e971688e2e53b", // Crypto.USDT/USD
  wsol: "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d", // Crypto.SOL/USD
  jitosol: "67be9f519b95cf24338801051f9a808eff0a578ccb388db73b7f6fe1de019ffb", // Crypto.JITOSOL/USD
  jupsol: "f8d8d6b6c866c8b2624fb5b679ae846738725e5fc887fa8e927c8d8645018a2b", // Crypto.JUPSOL/SOL.RR
  jupusd: "8ed858a2214e892c9371694fb6c8a9037b6ed4052c4edf209f8cb988484e81d9", // Crypto.JUPUSD/USD
  nvdax: "4244d07890e4610f46bbde67de8f43a4bf8b569eebe904f136b469f148503b7f", // Crypto.NVDAX/USD
  tslax: "47a156470288850a440df3a6ce85a55917b813a19bb5b31128a33a986566a362", // Crypto.TSLAX/USD
};

export const PYTH_SHARD_ID = 0;
export const PYTH_RECEIVER_PROGRAM_ID = new PublicKey("rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ");
const PYTH_PUSH_ORACLE_PROGRAM_ID = new PublicKey("pythWSnswVUd12oZpeFP8e9CVaEqJg25g1Vtc2biRsT");

/** Pyth's canonical price-feed account for a feed (shard 0): what the program reads prices from. */
export function pythFeedAccount(feedHex: string): PublicKey {
  const seedShard = Buffer.alloc(2);
  seedShard.writeUInt16LE(PYTH_SHARD_ID);
  return PublicKey.findProgramAddressSync([seedShard, Buffer.from(feedHex, "hex")], PYTH_PUSH_ORACLE_PROGRAM_ID)[0];
}

/** On-chain `AssetConfig.price_source` of each asset; everything not listed is plain Pyth. */
export type AssetPricing = { kind: "pyth" } | { kind: "scaledUiAmount" } | { kind: "redemptionRate"; base: AssetKey };
export const ASSET_PRICING: Record<AssetKey, AssetPricing> = {
  usdc: { kind: "pyth" },
  usdt: { kind: "pyth" },
  wsol: { kind: "pyth" },
  jitosol: { kind: "pyth" },
  jupsol: { kind: "redemptionRate", base: "wsol" },
  jupusd: { kind: "pyth" },
  nvdax: { kind: "scaledUiAmount" },
  tslax: { kind: "scaledUiAmount" },
};

/**
 * The extra account the program reads to value `asset`, or null for plain Pyth assets: the mint
 * itself for an xStock, the base feed's canonical account (SOL/USD) for JupSOL.
 */
export function priceSourceAccountFor(asset: AssetKey): PublicKey | null {
  const pricing = ASSET_PRICING[asset];
  if (pricing.kind === "scaledUiAmount") return ASSET_MINTS[asset];
  if (pricing.kind === "redemptionRate") return pythFeedAccount(PYTH_FEED_IDS[pricing.base]);
  return null;
}

export function feedIdToBytes(hex: string): number[] {
  const clean = hex.startsWith("0x") ? hex.slice(2) : hex;
  if (clean.length !== 64) throw new Error(`feed id must be 32 bytes (64 hex chars), got ${clean.length}`);
  const bytes: number[] = [];
  for (let i = 0; i < 64; i += 2) bytes.push(parseInt(clean.slice(i, i + 2), 16));
  return bytes;
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
