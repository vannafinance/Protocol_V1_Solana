#!/usr/bin/env node
import { PublicKey } from "@solana/web3.js";
import * as fs from "node:fs";
import * as path from "node:path";
import { devnetConnection, loadKeypair, log } from "./devnet-env";
import { callForkCheatcode } from "./devnet-pyth";
import { ORACLE, PROGRAM_ID, VALIDATOR } from "./pda";

const LOADER = new PublicKey("BPFLoaderUpgradeab1e11111111111111111111111");

const PROGRAMS: [string, PublicKey][] = [
  ["vanna_credit_layer", PROGRAM_ID],
  ["vanna_oracle", ORACLE],
  ["vanna_validator", VALIDATOR],
];

async function main() {
  const conn = devnetConnection();
  const authority = loadKeypair().publicKey;
  const u32 = (v: number) => {
    const b = Buffer.alloc(4);
    b.writeUInt32LE(v);
    return b;
  };
  for (const [name, programId] of PROGRAMS) {
    const elf = fs.readFileSync(path.resolve(__dirname, "..", "..", "target", "deploy", `${name}.so`));
    const [programData] = PublicKey.findProgramAddressSync([programId.toBuffer()], LOADER);
    const dataAccount = Buffer.concat([u32(3), Buffer.alloc(8), Buffer.from([1]), authority.toBuffer(), elf]);
    const programAccount = Buffer.concat([u32(2), programData.toBuffer()]);
    const write = async (key: PublicKey, data: Buffer, executable: boolean) =>
      callForkCheatcode(conn.rpcEndpoint, "surfnet_setAccount", [
        key.toBase58(),
        {
          lamports: await conn.getMinimumBalanceForRentExemption(data.length),
          data: data.toString("hex"),
          owner: LOADER.toBase58(),
          executable,
          rentEpoch: 0,
        },
      ]);
    await write(programData, dataAccount, false);
    await write(programId, programAccount, true);
    log(`deployed ${name}`, `${programId.toBase58()} (${(elf.length / 1024).toFixed(0)} KiB)`);
  }
}

main().catch((err) => {
  console.error(err instanceof Error ? err.message : err);
  process.exit(1);
});
