import {
  AddressLookupTableAccount,
  AddressLookupTableProgram,
  Connection,
  Keypair,
  PublicKey,
  TransactionInstruction,
  TransactionMessage,
  VersionedTransaction,
} from "@solana/web3.js";

const PACKET_DATA_SIZE = 1232;

export async function sendV0(conn: Connection, wallet: Keypair, ixs: TransactionInstruction[], existing: AddressLookupTableAccount[] = []): Promise<string> {
  const build = async (instructions: TransactionInstruction[], tables: AddressLookupTableAccount[]) => {
    const message = new TransactionMessage({
      payerKey: wallet.publicKey,
      recentBlockhash: (await conn.getLatestBlockhash()).blockhash,
      instructions,
    }).compileToV0Message(tables);
    const tx = new VersionedTransaction(message);
    tx.sign([wallet]);
    return tx;
  };
  const send = async (tx: VersionedTransaction) => {
    const sig = await conn.sendTransaction(tx);
    const result = await conn.confirmTransaction(sig, "confirmed");
    if (result.value.err) throw new Error(`transaction ${sig} failed: ${JSON.stringify(result.value.err)}`);
    return sig;
  };

  const direct = await build(ixs, existing).catch(() => null);
  if (direct && direct.serialize().length <= PACKET_DATA_SIZE) return send(direct);

  const inExisting = (k: PublicKey) => existing.some((t) => t.state.addresses.some((a) => a.equals(k)));
  const keys: PublicKey[] = [];
  for (const ix of ixs) {
    for (const k of [ix.programId, ...ix.keys.map((m) => m.pubkey)]) {
      if (!k.equals(wallet.publicKey) && !inExisting(k) && !keys.some((e) => e.equals(k))) keys.push(k);
    }
  }
  const [create, table] = AddressLookupTableProgram.createLookupTable({
    authority: wallet.publicKey,
    payer: wallet.publicKey,
    recentSlot: (await conn.getSlot("confirmed")) - 1,
  });
  await send(await build([create], []));
  for (let i = 0; i < keys.length; i += 25) {
    const extend = AddressLookupTableProgram.extendLookupTable({
      lookupTable: table,
      authority: wallet.publicKey,
      payer: wallet.publicKey,
      addresses: keys.slice(i, i + 25),
    });
    await send(await build([extend], []));
  }

  const extendedAt = await conn.getSlot("confirmed");
  while ((await conn.getSlot("confirmed")) <= extendedAt) await new Promise((r) => setTimeout(r, 200));
  const account = (await conn.getAddressLookupTable(table)).value;
  if (!account) throw new Error("lookup table not found");
  return send(await build(ixs, [...existing, account]));
}
