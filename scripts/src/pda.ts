import { PublicKey } from "@solana/web3.js";

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

export function marginPda(authority: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([enc("margin"), authority.toBuffer()], PROGRAM_ID);
}

export function debtPositionPda(margin: PublicKey, reserve: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([enc("debt"), margin.toBuffer(), reserve.toBuffer()], PROGRAM_ID);
}

export function integrationPda(programId: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([enc("integration"), programId.toBuffer()], PROGRAM_ID);
}

export const ORACLE = new PublicKey("FXY5DRfekMUTbUp4uyCFpCZmCccrp6kPq3hTM4GRihnc");
export const VALIDATOR = new PublicKey("6fND3vhtstp486iE6rsSNVUox3kcjNNRsSLAN7ZPs7th");

export function priceBookPda(): PublicKey {
  return PublicKey.findProgramAddressSync([enc("price_book")], ORACLE)[0];
}

export function marketBookPda(venue: PublicKey): PublicKey {
  return PublicKey.findProgramAddressSync([enc("market_book"), venue.toBuffer()], ORACLE)[0];
}

export function venueAccountPda(margin: PublicKey, venue: PublicKey): PublicKey {
  return PublicKey.findProgramAddressSync([enc("venue_account"), margin.toBuffer(), venue.toBuffer()], PROGRAM_ID)[0];
}
