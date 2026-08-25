import * as anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey } from "@solana/web3.js";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

/**
 * Shared environment for every script in `scripts/src/devnet/`: wallet loading, the Devnet RPC
 * connection, an Anchor `Program` bound to that wallet, and the two real assets these scripts use.
 *
 * Real assets only — Circle's official Devnet USDC and the native SOL mint (wrapped as WSOL) — so
 * that real Pyth price feeds apply directly (see `devnet-pyth.ts`). There is no "create a test
 * mint" helper here: these mints already exist on Devnet and nobody but their real authorities can
 * mint them, so funding a wallet with them goes through the real faucets documented in
 * `COMMANDS.md`, not a script.
 */

export const DEVNET_RPC_URL = process.env.DEVNET_RPC_URL ?? "https://api.devnet.solana.com";

export type AssetKey = "usdc" | "wsol";

export const USDC_MINT = new PublicKey("4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU");
export const WSOL_MINT = new PublicKey("So11111111111111111111111111111111111111112");

export const ASSET_MINTS: Record<AssetKey, PublicKey> = { usdc: USDC_MINT, wsol: WSOL_MINT };
export const ASSET_DECIMALS: Record<AssetKey, number> = { usdc: 6, wsol: 9 };

/**
 * Pyth price feed IDs (verified live against Hermes's `/v2/price_feeds` endpoint — these are
 * chain-agnostic symbol identifiers, the same value on every chain Pyth publishes to).
 */
export const PYTH_FEED_IDS: Record<AssetKey, string> = {
  usdc: "eaa020c61cc479712813461ce153894a96a6c00b21ed0cfc2798d1f9a9e9c94a",
  wsol: "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d",
};

/** Shard ID for Pyth's long-lived "price feed accounts" — 0 is the default/only shard we use. */
export const PYTH_SHARD_ID = 0;

/** Converts a 64-char hex feed ID into the `[u8; 32]` array `admin_register_asset` expects. */
export function feedIdToBytes(hex: string): number[] {
  const clean = hex.startsWith("0x") ? hex.slice(2) : hex;
  if (clean.length !== 64) throw new Error(`feed id must be 32 bytes (64 hex chars), got ${clean.length}`);
  const bytes: number[] = [];
  for (let i = 0; i < 64; i += 2) bytes.push(parseInt(clean.slice(i, i + 2), 16));
  return bytes;
}

export function assetKeyFromString(value: string): AssetKey {
  if (value === "usdc" || value === "wsol") return value;
  throw new Error(`unknown asset "${value}" — expected "usdc" or "wsol"`);
}

function loadIdl(): anchor.Idl {
  const idlPath = path.resolve(__dirname, "..", "..", "target", "idl", "vanna_lending.json");
  return JSON.parse(fs.readFileSync(idlPath, "utf8")) as anchor.Idl;
}

export const IDL = loadIdl();

/**
 * Loads the signing keypair for these scripts: `--wallet <path>` (handled by callers via
 * `devnet-cli.ts`'s `parseArgs`) takes priority, then `ANCHOR_WALLET`, then the Solana CLI's own
 * default keypair path — the same wallet `solana` and `anchor` commands use by default.
 */
export function loadKeypair(explicitPath?: string): Keypair {
  const walletPath =
    explicitPath ?? process.env.ANCHOR_WALLET ?? path.join(os.homedir(), ".config", "solana", "id.json");
  const secret = JSON.parse(fs.readFileSync(walletPath, "utf8"));
  return Keypair.fromSecretKey(Uint8Array.from(secret));
}

export function devnetConnection(): Connection {
  return new Connection(DEVNET_RPC_URL, "confirmed");
}

/** One `Program` instance bound to `wallet` as the fee payer / default signer. */
export function programAs(connection: Connection, wallet: Keypair): anchor.Program {
  const anchorWallet = new anchor.Wallet(wallet);
  const provider = new anchor.AnchorProvider(connection, anchorWallet, { commitment: "confirmed" });
  return new anchor.Program(IDL, provider);
}

export function log(step: string, detail?: string): void {
  console.log(detail ? `✔ ${step} — ${detail}` : `✔ ${step}`);
}
