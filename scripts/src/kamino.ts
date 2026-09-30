import { TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { AccountMeta, Connection, PublicKey } from "@solana/web3.js";
import { ata } from "./devnet-cli";
import { AssetKey, ASSET_MINTS, tokenProgramFor } from "./devnet-env";

export const KLEND = new PublicKey("KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD");
export const INSTRUCTIONS_SYSVAR = new PublicKey("Sysvar1nstructions1111111111111111111111111");

const MAIN_MARKET = new PublicKey("7u3HeHxYDLhnCoErrtycNokbQYbWGzLs6JSDqGAv5PfF");
const MAIN_MARKET_AUTHORITY = new PublicKey("9DrvZvyWh1HuAoZxvYWMvkf2XCzryCpGgHqrMjyDWpmo");

export const CTOKEN_DECIMALS = 6;

export type ReceiptKey = "kusdc" | "kwsol";

export interface KaminoReceipt {
  underlying: AssetKey;
  market: PublicKey;
  marketAuthority: PublicKey;
  reserve: PublicKey;
  liquidityVault: PublicKey;
  collateralMint: PublicKey;
}

export const KAMINO_RECEIPTS: Record<ReceiptKey, KaminoReceipt> = {
  kusdc: {
    underlying: "usdc",
    market: MAIN_MARKET,
    marketAuthority: MAIN_MARKET_AUTHORITY,
    reserve: new PublicKey("D6q6wuQSrifJKZYpR1M8R4YawnLDtDsMmWM1NbBmgJ59"),
    liquidityVault: new PublicKey("Bgq7trRgVMeq33yt235zM2onQ4bRDBsY5EWiTetF4qw6"),
    collateralMint: new PublicKey("B8V6WVjPxW1UGwVDfxH2d2r8SyT4cqn7dQRK6XneVa7D"),
  },
  kwsol: {
    underlying: "wsol",
    market: MAIN_MARKET,
    marketAuthority: MAIN_MARKET_AUTHORITY,
    reserve: new PublicKey("d4A2prbA2whesmvHaL88BH6Ewn5N4bTSU2Ze8P6Bc4Q"),
    liquidityVault: new PublicKey("GafNuUXj9rxGLn4y79dPu6MHSuPWeJR6UtTWuexpGh3U"),
    collateralMint: new PublicKey("2UywZrUdyqs5vDchy7fKQJKau2RVyuzBev2XKGPDSiX1"),
  },
};

export function receiptKeyFromString(value: string): ReceiptKey {
  const v = value.toLowerCase().replace(/^k/, "");
  if (v === "usdc") return "kusdc";
  if (v === "wsol" || v === "sol") return "kwsol";
  throw new Error(`unknown Kamino receipt "${value}" — expected USDC|SOL`);
}

const DEPOSIT_RESERVE_LIQUIDITY = Buffer.from([169, 201, 30, 126, 6, 205, 102, 68]);
const REDEEM_RESERVE_COLLATERAL = Buffer.from([234, 117, 181, 125, 185, 142, 220, 29]);

export type KaminoCall = "deposit" | "redeem";

export function kaminoCallData(call: KaminoCall, amount: bigint): Buffer {
  const data = Buffer.alloc(16);
  (call === "deposit" ? DEPOSIT_RESERVE_LIQUIDITY : REDEEM_RESERVE_COLLATERAL).copy(data, 0);
  data.writeBigUInt64LE(amount, 8);
  return data;
}

export function kaminoCallAccounts(call: KaminoCall, receipt: KaminoReceipt, margin: PublicKey): AccountMeta[] {
  const underlyingMint = ASSET_MINTS[receipt.underlying];
  const liquidityProgram = tokenProgramFor(receipt.underlying);
  const marginLiquidity = ata(margin, underlyingMint, liquidityProgram);
  const marginCollateral = ata(margin, receipt.collateralMint, TOKEN_PROGRAM_ID);
  const w = (pubkey: PublicKey) => ({ pubkey, isWritable: true, isSigner: false });
  const r = (pubkey: PublicKey) => ({ pubkey, isWritable: false, isSigner: false });
  const tail = [r(TOKEN_PROGRAM_ID), r(liquidityProgram), r(INSTRUCTIONS_SYSVAR)];
  return call === "deposit"
    ? [
        w(margin),
        w(receipt.reserve),
        r(receipt.market),
        r(receipt.marketAuthority),
        r(underlyingMint),
        w(receipt.liquidityVault),
        w(receipt.collateralMint),
        w(marginLiquidity),
        w(marginCollateral),
        ...tail,
      ]
    : [
        w(margin),
        r(receipt.market),
        w(receipt.reserve),
        r(receipt.marketAuthority),
        r(underlyingMint),
        w(receipt.collateralMint),
        w(receipt.liquidityVault),
        w(marginCollateral),
        w(marginLiquidity),
        ...tail,
      ];
}

export async function receiptUnderlying(conn: Connection, receipt: KaminoReceipt, receipts: bigint): Promise<bigint> {
  if (receipts === 0n) return 0n;
  const info = await conn.getAccountInfo(receipt.reserve);
  if (!info) throw new Error(`Kamino reserve ${receipt.reserve.toBase58()} not found`);
  const d = info.data;
  const u128 = (i: number) => d.readBigUInt64LE(i) + (d.readBigUInt64LE(i + 8) << 64n);
  const totalSf = (d.readBigUInt64LE(224) << 60n) + u128(232) - u128(344) - u128(360) - u128(376);
  const supply = d.readBigUInt64LE(2592);
  return supply === 0n ? 0n : (receipts * (totalSf >> 60n)) / supply;
}
