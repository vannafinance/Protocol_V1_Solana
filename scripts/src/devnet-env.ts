import * as anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey } from "@solana/web3.js";
import { TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID } from "@solana/spl-token";
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

export type AssetKey = "usdc" | "wsol" | "tslax" | "googlx";

/** Real mainnet USDC (cloned onto the Surfpool fork by address). */
export const USDC_MINT = new PublicKey("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
export const WSOL_MINT = new PublicKey("So11111111111111111111111111111111111111112");
export const TSLAX_MINT = new PublicKey("XsDoVfqeBukxuZHWhdvWHBhgEHjGNst4MLodqsJHzoB");
export const GOOGLX_MINT = new PublicKey("XsCPL9dNWBMvFtTmwcCA5v3xWPSMEBCszbQdiLLq6aN");

export const ASSET_MINTS: Record<AssetKey, PublicKey> = {
  usdc: USDC_MINT,
  wsol: WSOL_MINT,
  tslax: TSLAX_MINT,
  googlx: GOOGLX_MINT,
};
export const ASSET_DECIMALS: Record<AssetKey, number> = {
  usdc: 6,
  wsol: 9,
  tslax: 8,
  googlx: 8,
};

export const ASSET_TOKEN_PROGRAM: Record<AssetKey, PublicKey> = {
  usdc: TOKEN_PROGRAM_ID,
  wsol: TOKEN_PROGRAM_ID,
  tslax: TOKEN_2022_PROGRAM_ID,
  googlx: TOKEN_2022_PROGRAM_ID,
};

/**
 * Pyth price feed IDs (chain-agnostic; same hex on every cluster).
 */
export const PYTH_FEED_IDS: Record<AssetKey, string> = {
  usdc: "eaa020c61cc479712813461ce153894a96a6c00b21ed0cfc2798d1f9a9e9c94a",
  wsol: "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d",
  tslax: "e6da44bff5b8b06897a3739dd331b440d6662595bb862e37046892c568ae3fc0",
  googlx: "ad519718d387de4f0d7d29ea16a3730ce42e49c59fef6fba6fc9bac477645f6f",
};

export const PYTH_SHARD_ID = 0;

export function feedIdToBytes(hex: string): number[] {
  const clean = hex.startsWith("0x") ? hex.slice(2) : hex;
  if (clean.length !== 64) throw new Error(`feed id must be 32 bytes (64 hex chars), got ${clean.length}`);
  const bytes: number[] = [];
  for (let i = 0; i < 64; i += 2) bytes.push(parseInt(clean.slice(i, i + 2), 16));
  return bytes;
}

export function assetKeyFromString(value: string): AssetKey {
  const v = value.toLowerCase();
  if (v === "usdc" || v === "wsol" || v === "tslax" || v === "googlx") return v;
  throw new Error(`unknown asset "${value}" — expected usdc|wsol|tslax|googlx`);
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
