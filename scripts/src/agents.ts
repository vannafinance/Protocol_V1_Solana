import * as anchor from "@coral-xyz/anchor";
import { AccountMeta, Connection, PublicKey, SystemProgram, TransactionInstruction } from "@solana/web3.js";
import { AssetKey, KlendRate, log, oracleAccountsFor, oracleConfigArg, agentAs } from "./devnet-env";
import { oracleMetas } from "./oracle";
import { ORACLE, priceBookPda, protocolConfigPda } from "./pda";

export function agentSegment(program: PublicKey, accounts: PublicKey[]): AccountMeta[] {
  return [{ pubkey: program, isWritable: false, isSigner: false }, ...oracleMetas(accounts)];
}

export function oracleSegment(accounts: PublicKey[]): AccountMeta[] {
  return agentSegment(ORACLE, [priceBookPda(), ...accounts]);
}

export async function ensurePriceBook(conn: Connection, admin: anchor.web3.Keypair): Promise<void> {
  if (await conn.getAccountInfo(priceBookPda())) return;
  const oracle = agentAs(conn, admin, "vanna_oracle");
  const sig = await oracle.methods
    .openPriceBook()
    .accounts({
      admin: admin.publicKey,
      payer: admin.publicKey,
      protocolConfig: protocolConfigPda()[0],
      priceBook: priceBookPda(),
      systemProgram: SystemProgram.programId,
    })
    .rpc();
  log("open_price_book", `${priceBookPda().toBase58()} tx=${sig}`);
}

export async function setPriceSourceIx(
  conn: Connection,
  admin: anchor.web3.Keypair,
  mint: PublicKey,
  asset: AssetKey,
  klend?: KlendRate,
): Promise<TransactionInstruction> {
  const oracle = agentAs(conn, admin, "vanna_oracle");
  return oracle.methods
    .setPriceSource(oracleConfigArg(asset, klend))
    .accounts({ admin: admin.publicKey, protocolConfig: protocolConfigPda()[0], priceBook: priceBookPda(), mint })
    .remainingAccounts(oracleMetas(priceAccounts(asset, klend)))
    .instruction();
}

export function priceAccounts(asset: AssetKey, klend?: KlendRate): PublicKey[] {
  return [...oracleAccountsFor(asset), ...(klend ? [klend.reserve] : [])];
}
