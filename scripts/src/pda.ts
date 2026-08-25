import { PublicKey } from "@solana/web3.js";

/** Mirrors programs/vanna_lending/src/constants.rs seeds exactly. */
export const PROGRAM_ID = new PublicKey("BZ812nUv4Qhr2p1JVgmoJGjYTGk1brAXckyhFSCNH3Zg");

const enc = (s: string) => Buffer.from(s, "utf8");

export function protocolConfigPda(): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([enc("protocol")], PROGRAM_ID);
}

export function assetConfigPda(mint: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([enc("asset"), mint.toBuffer()], PROGRAM_ID);
}

export function reservePda(mint: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([enc("reserve"), mint.toBuffer()], PROGRAM_ID);
}

export function shareMintPda(mint: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([enc("share_mint"), mint.toBuffer()], PROGRAM_ID);
}

/** One margin account per wallet — seeded only by authority, no subaccount id. */
export function marginPda(authority: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([enc("margin"), authority.toBuffer()], PROGRAM_ID);
}

export function debtPositionPda(margin: PublicKey, reserve: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([enc("debt"), margin.toBuffer(), reserve.toBuffer()], PROGRAM_ID);
}
