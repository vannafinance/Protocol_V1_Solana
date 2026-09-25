import * as anchor from "@coral-xyz/anchor";
import { getAssociatedTokenAddressSync } from "@solana/spl-token";
import { Connection, PublicKey } from "@solana/web3.js";

/**
 * Minimal `--flag value` / `--flag` (boolean) parser — no external CLI framework needed.
 * Pass the flag portion of argv explicitly (e.g. everything after the subcommand name); defaults
 * to `process.argv.slice(2)` for callers with no subcommand of their own.
 */
export function parseArgs(argv: string[] = process.argv.slice(2)): Record<string, string> {
  const out: Record<string, string> = {};
  for (let i = 0; i < argv.length; i++) {
    const token = argv[i];
    if (!token.startsWith("--")) continue;
    const key = token.slice(2);
    const next = argv[i + 1];
    if (next !== undefined && !next.startsWith("--")) {
      out[key] = next;
      i++;
    } else {
      out[key] = "true";
    }
  }
  return out;
}

export function requireArg(args: Record<string, string>, key: string): string {
  const value = args[key];
  if (value === undefined) {
    throw new Error(`missing required --${key} argument`);
  }
  return value;
}

export function optionalArg(args: Record<string, string>, key: string, fallback: string): string {
  return args[key] ?? fallback;
}

/**
 * Converts a human-readable decimal amount (e.g. "12.5") to raw base units for a mint with
 * `decimals` decimal places, as an exact integer string operation — no floating point, so large or
 * precise amounts round-trip exactly.
 */
export function toBaseUnits(humanAmount: string, decimals: number): anchor.BN {
  const negative = humanAmount.startsWith("-");
  const unsigned = negative ? humanAmount.slice(1) : humanAmount;
  const [wholePart, fracPart = ""] = unsigned.split(".");
  if (fracPart.length > decimals) {
    throw new Error(`amount "${humanAmount}" has more than ${decimals} decimal places`);
  }
  const fracPadded = fracPart.padEnd(decimals, "0");
  const combined = `${wholePart || "0"}${fracPadded}`.replace(/^0+(?=\d)/, "");
  const bn = new anchor.BN(combined || "0");
  return negative ? bn.neg() : bn;
}

export function toBigInt(bn: anchor.BN): bigint {
  return BigInt(bn.toString());
}

export function ata(owner: PublicKey, mint: PublicKey, tokenProgram?: PublicKey): PublicKey {
  return getAssociatedTokenAddressSync(mint, owner, true, tokenProgram);
}

export async function tokenBalance(connection: Connection, tokenAccount: PublicKey): Promise<bigint> {
  const info = await connection.getTokenAccountBalance(tokenAccount, "confirmed");
  return BigInt(info.value.amount);
}
