#!/usr/bin/env node
/**
 * Surfpool fork helpers: xStock registration and funding, plus the external integrations that
 * go through `margin_execute`:
 *
 *   Kamino:  `register-kamino`, `register-receipt --symbol X`, then
 *            `kamino-deposit` / `kamino-redeem --symbol X --amount N`
 *   Jupiter: `register-jupiter`, then `jupiter-swap --from SOL --to USDC --amount N`
 *
 *   npx tsx src/integrations-fork.ts <command> [--flag value ...]
 */
import * as anchor from "@coral-xyz/anchor";
import {
  ASSOCIATED_TOKEN_PROGRAM_ID,
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  getAssociatedTokenAddressSync,
} from "@solana/spl-token";
import {
  AddressLookupTableAccount,
  ComputeBudgetProgram,
  Connection,
  PublicKey,
  SystemProgram,
  TransactionMessage,
  VersionedTransaction,
} from "@solana/web3.js";
import { spawnSync } from "node:child_process";
import { ata, optionalArg, parseArgs, requireArg, toBaseUnits } from "./devnet-cli";
import {
  ASSET_DECIMALS,
  ASSET_MINTS,
  AssetKey,
  DEVNET_RPC_URL,
  assetKeyFromString,
  devnetConnection,
  feedIdToBytes,
  loadKeypair,
  log,
  programAs,
  PYTH_FEED_IDS,
  tokenProgramFor,
} from "./devnet-env";
import { buildRemainingAccounts } from "./devnet-positions";
import { JUPITER, jupiterRouteForMargin } from "./jupiter";
import { refreshPrice } from "./devnet-pyth";
import {
  CTOKEN_DECIMALS,
  KAMINO_RECEIPTS,
  KaminoCall,
  kaminoCallAccounts,
  kaminoCallData,
  KLEND,
  receiptKeyFromString,
} from "./kamino";
import { assetConfigPda, integrationPda, marginPda, protocolConfigPda, reservePda, shareMintPda } from "./pda";

type Ctx = {
  args: Record<string, string>;
  conn: Connection;
  wallet: anchor.web3.Keypair;
  program: anchor.Program;
};

function method(program: anchor.Program, snake: string, camel: string) {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const methods = program.methods as Record<string, (...args: unknown[]) => any>;
  if (typeof methods[camel] === "function") return methods[camel].bind(methods);
  if (typeof methods[snake] === "function") return methods[snake].bind(methods);
  throw new Error(`IDL is missing ${camel} / ${snake} — rebuild the program and refresh the IDL`);
}

async function callCheatcode(methodName: string, params: unknown[]): Promise<void> {
  const response = await fetch(DEVNET_RPC_URL, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: methodName, params }),
  });
  const body = (await response.json()) as { error?: { message: string } };
  if (body.error) throw new Error(`${methodName}: ${body.error.message}`);
}

function stockKey(symbol: string): "tslax" | "googlx" {
  const v = symbol.toLowerCase();
  if (v === "tslax" || v === "tsla") return "tslax";
  if (v === "googlx" || v === "googl") return "googlx";
  throw new Error(`unsupported stock "${symbol}" — use TSLAX or GOOGLX`);
}

/** Runs a `devnet.ts` subcommand in a child process, streaming its output. */
function runDevnet(...devnetArgs: string[]) {
  return spawnSync("npx", ["tsx", "src/devnet.ts", ...devnetArgs], {
    cwd: __dirname + "/..",
    stdio: "inherit",
    env: process.env,
  });
}

function assetFromSymbolArg(args: Record<string, string>): AssetKey {
  return assetKeyFromString((args.symbol ?? "TSLAX").toLowerCase());
}

const commands: Record<string, (ctx: Ctx) => Promise<void>> = {
  // -- delegates to devnet.ts ---------------------------------------------------------------------
  "register-asset": async ({ args }) => {
    const asset = assetFromSymbolArg(args);
    log("delegating", `devnet.ts register-asset --asset ${asset}`);
    const r = runDevnet("register-asset", "--asset", asset);
    if (r.status !== 0) process.exit(r.status ?? 1);
  },

  "init-reserve": async ({ args }) => {
    const asset = assetFromSymbolArg(args);
    const r = runDevnet("initialize-reserve", "--asset", asset);
    if (r.status !== 0) process.exit(r.status ?? 1);
  },

  "register-xstocks": async () => {
    for (const asset of ["tslax", "googlx", "aaplx"] as AssetKey[]) {
      runDevnet("register-asset", "--asset", asset);
      runDevnet("initialize-reserve", "--asset", asset);
    }
    log("register-xstocks", "TSLAx + GOOGLx + AAPLx registered (or already present)");
  },

  // -- fork funding (Surfpool cheatcodes / airdrop) -----------------------------------------------
  "fund-sol": async ({ args, conn }) => {
    const to = new PublicKey(requireArg(args, "to"));
    const amount = Number(optionalArg(args, "amount", "10"));
    const sig = await conn.requestAirdrop(to, amount * 1e9);
    await conn.confirmTransaction(sig, "confirmed");
    log("fund-sol", `${amount} SOL → ${to.toBase58()} tx=${sig}`);
  },

  "fund-usdc": async ({ args }) => {
    const to = new PublicKey(requireArg(args, "to"));
    const amount = Number(optionalArg(args, "amount", "1000"));
    const raw = amount * 10 ** ASSET_DECIMALS.usdc;
    await callCheatcode("surfnet_setTokenAccount", [
      to.toBase58(),
      ASSET_MINTS.usdc.toBase58(),
      { amount: raw },
      TOKEN_PROGRAM_ID.toBase58(),
    ]);
    log("fund-usdc", `+${amount} USDC → ${to.toBase58()}`);
  },

  "fund-tslax": async ({ args }) => {
    const to = new PublicKey(requireArg(args, "to"));
    const amount = Number(optionalArg(args, "amount", "100"));
    const raw = amount * 10 ** ASSET_DECIMALS.tslax;
    await callCheatcode("surfnet_setTokenAccount", [
      to.toBase58(),
      ASSET_MINTS.tslax.toBase58(),
      { amount: raw },
      TOKEN_2022_PROGRAM_ID.toBase58(),
    ]);
    log("fund-tslax", `+${amount} TSLAx → ${to.toBase58()}`);
  },

  "fund-xstock": async ({ args }) => {
    const key = stockKey(requireArg(args, "symbol"));
    const to = new PublicKey(requireArg(args, "to"));
    const amount = Number(optionalArg(args, "amount", "50"));
    const raw = amount * 10 ** ASSET_DECIMALS[key];
    await callCheatcode("surfnet_setTokenAccount", [
      to.toBase58(),
      ASSET_MINTS[key].toBase58(),
      { amount: raw },
      tokenProgramFor(key).toBase58(),
    ]);
    log("fund-xstock", `+${amount} ${key} → ${to.toBase58()}`);
  },

  // -- lender / margin ----------------------------------------------------------------------------
  "supply-tslax": async ({ args, wallet, program }) => {
    const amount = optionalArg(args, "amount", "10");
    const mint = ASSET_MINTS.tslax;
    const raw = toBaseUnits(amount, ASSET_DECIMALS.tslax);
    const [protocolConfig] = protocolConfigPda();
    const [assetConfig] = assetConfigPda(mint);
    const [reserve] = reservePda(mint);
    const [shareMint] = shareMintPda(mint);
    const lenderAta = getAssociatedTokenAddressSync(mint, wallet.publicKey, false, TOKEN_2022_PROGRAM_ID);
    const vault = getAssociatedTokenAddressSync(mint, reserve, true, TOKEN_2022_PROGRAM_ID);
    const shareAta = getAssociatedTokenAddressSync(shareMint, wallet.publicKey, false, TOKEN_PROGRAM_ID);
    const sig = await method(program, "lender_supply", "lenderSupply")(raw, new anchor.BN(1))
      .accounts({
        lender: wallet.publicKey,
        protocolConfig,
        assetConfig,
        reserve,
        underlyingMint: mint,
        lenderTokenAccount: lenderAta,
        liquidityVault: vault,
        shareMint,
        lenderShareAccount: shareAta,
        tokenProgram: TOKEN_2022_PROGRAM_ID,
        shareTokenProgram: TOKEN_PROGRAM_ID,
        associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .rpc();
    log("supply-tslax", `${amount} TSLAx tx=${sig}`);
  },

  "supply-xstock": async ({ args, wallet, program }) => {
    const key = stockKey(requireArg(args, "symbol"));
    const amount = optionalArg(args, "amount", "20");
    const mint = ASSET_MINTS[key];
    const tp = tokenProgramFor(key);
    const raw = toBaseUnits(amount, ASSET_DECIMALS[key]);
    const [protocolConfig] = protocolConfigPda();
    const [assetConfig] = assetConfigPda(mint);
    const [reserve] = reservePda(mint);
    const [shareMint] = shareMintPda(mint);
    const lenderAta = getAssociatedTokenAddressSync(mint, wallet.publicKey, false, tp);
    const vault = getAssociatedTokenAddressSync(mint, reserve, true, tp);
    const shareAta = getAssociatedTokenAddressSync(shareMint, wallet.publicKey, false, TOKEN_PROGRAM_ID);
    const sig = await method(program, "lender_supply", "lenderSupply")(raw, new anchor.BN(1))
      .accounts({
        lender: wallet.publicKey,
        protocolConfig,
        assetConfig,
        reserve,
        underlyingMint: mint,
        lenderTokenAccount: lenderAta,
        liquidityVault: vault,
        shareMint,
        lenderShareAccount: shareAta,
        tokenProgram: tp,
        shareTokenProgram: TOKEN_PROGRAM_ID,
        associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .rpc();
    log("supply-xstock", `${amount} ${key} tx=${sig}`);
  },

  "create-margin": async ({ wallet, program }) => {
    const [protocolConfig] = protocolConfigPda();
    const [marginAccount] = marginPda(wallet.publicKey);
    const sig = await method(program, "user_create_margin", "userCreateMargin")()
      .accounts({
        payer: wallet.publicKey,
        authority: wallet.publicKey,
        protocolConfig,
        marginAccount,
        systemProgram: SystemProgram.programId,
      })
      .rpc();
    log("create-margin", `${marginAccount.toBase58()} tx=${sig}`);
  },

  // -- Kamino via the generic external-call path ----------------------------------------------------
  "register-kamino": async ({ wallet, program }) => {
    const [protocolConfig] = protocolConfigPda();
    const [integration] = integrationPda(KLEND);
    const sig = await method(program, "admin_register_integration", "adminRegisterIntegration")({ kaminoLend: {} })
      .accounts({
        admin: wallet.publicKey,
        payer: wallet.publicKey,
        protocolConfig,
        targetProgram: KLEND,
        integration,
        systemProgram: SystemProgram.programId,
      })
      .rpc();
    log("admin_register_integration", `klend integration=${integration.toBase58()} tx=${sig}`);
  },

  "set-kamino-enabled": async ({ args, wallet, program }) => {
    const enabled = optionalArg(args, "enabled", "true") === "true";
    const [protocolConfig] = protocolConfigPda();
    const [integration] = integrationPda(KLEND);
    const sig = await method(program, "admin_set_integration_enabled", "adminSetIntegrationEnabled")(enabled)
      .accounts({ admin: wallet.publicKey, protocolConfig, integration })
      .rpc();
    log("admin_set_integration_enabled", `klend enabled=${enabled} tx=${sig}`);
  },

  /**
   * Registers a Kamino cToken as collateral priced by its reserve's rate × the underlying price.
   * Order matters: register disabled, attach the price source, then enable, so the cToken is never
   * live as collateral under plain Pyth pricing.
   */
  "register-receipt": async ({ args, wallet, program }) => {
    const key = receiptKeyFromString(requireArg(args, "symbol"));
    const receipt = KAMINO_RECEIPTS[key];
    const [protocolConfig] = protocolConfigPda();
    const [assetConfig] = assetConfigPda(receipt.collateralMint);
    const risk = [
      Number(optionalArg(args, "ltv-bps", "5000")),
      Number(optionalArg(args, "liq-threshold-bps", "6000")),
      Number(optionalArg(args, "liq-bonus-bps", "500")),
      Number(optionalArg(args, "max-confidence-bps", "1000")),
      Number(optionalArg(args, "max-price-age-secs", "3600")),
    ];
    const maxCollateral = toBaseUnits(optionalArg(args, "max-collateral", "0"), CTOKEN_DECIMALS);
    const registerSig = await method(program, "admin_register_asset", "adminRegisterAsset")(
      feedIdToBytes(PYTH_FEED_IDS[receipt.underlying]),
      maxCollateral,
      ...risk,
      false,
      false,
    )
      .accounts({
        admin: wallet.publicKey,
        payer: wallet.publicKey,
        protocolConfig,
        underlyingMint: receipt.collateralMint,
        assetConfig,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .rpc();
    const sourceSig = await method(program, "admin_set_asset_price_source", "adminSetAssetPriceSource")(
      { kaminoReceipt: {} },
      KLEND,
    )
      .accounts({
        admin: wallet.publicKey,
        protocolConfig,
        assetConfig,
        sourceAccount: receipt.reserve,
        underlyingAssetConfig: assetConfigPda(ASSET_MINTS[receipt.underlying])[0],
      })
      .rpc();
    const enableSig = await method(program, "admin_update_asset_config", "adminUpdateAssetConfig")(
      maxCollateral,
      ...risk,
      true,
      false,
    )
      .accounts({ admin: wallet.publicKey, protocolConfig, assetConfig })
      .rpc();
    log("register-receipt", `${key} assetConfig=${assetConfig.toBase58()} txs=${registerSig},${sourceSig},${enableSig}`);
  },

  "kamino-deposit": async (ctx) => kaminoCall(ctx, "deposit"),
  "kamino-redeem": async (ctx) => kaminoCall(ctx, "redeem"),

  // -- Jupiter via the generic external-call path ---------------------------------------------------
  "register-jupiter": async ({ wallet, program }) => {
    const [protocolConfig] = protocolConfigPda();
    const [integration] = integrationPda(JUPITER);
    const sig = await method(program, "admin_register_integration", "adminRegisterIntegration")({ jupiter: {} })
      .accounts({
        admin: wallet.publicKey,
        payer: wallet.publicKey,
        protocolConfig,
        targetProgram: JUPITER,
        integration,
        systemProgram: SystemProgram.programId,
      })
      .rpc();
    log("admin_register_integration", `jupiter integration=${integration.toBase58()} tx=${sig}`);
  },

  /**
   * Swaps margin collateral through Jupiter via `margin_execute`. `--min-received` defaults to
   * Jupiter's own slippage floor for the quote.
   */
  "jupiter-swap": async ({ args, wallet, program, conn }) => {
    const from = assetKeyFromString(requireArg(args, "from"));
    const to = assetKeyFromString(requireArg(args, "to"));
    const amount = BigInt(toBaseUnits(requireArg(args, "amount"), ASSET_DECIMALS[from]).toString());
    const [margin] = marginPda(wallet.publicKey);
    const swap = await jupiterRouteForMargin({
      inputMint: ASSET_MINTS[from],
      outputMint: ASSET_MINTS[to],
      amount,
      margin,
      slippageBps: Number(optionalArg(args, "slippage-bps", "50")),
    });
    const minReceived = args["min-received"]
      ? toBaseUnits(args["min-received"], ASSET_DECIMALS[to])
      : new anchor.BN(swap.minOut.toString());

    const prices = await refreshAllPrices(conn, wallet);
    const health = await buildRemainingAccounts(program, margin, prices, { excludeCollateral: [from, to] });
    const [protocolConfig] = protocolConfigPda();
    const ix = await method(program, "margin_execute", "marginExecute")(swap.data, swap.accounts.length, minReceived)
      .accounts({
        authority: wallet.publicKey,
        protocolConfig,
        marginAccount: margin,
        integration: integrationPda(JUPITER)[0],
        targetProgram: JUPITER,
        spentAsset: assetConfigPda(ASSET_MINTS[from])[0],
        spentMint: ASSET_MINTS[from],
        spentVault: ata(margin, ASSET_MINTS[from], tokenProgramFor(from)),
        spentPriceUpdate: prices[from],
        receivedAsset: assetConfigPda(ASSET_MINTS[to])[0],
        receivedMint: ASSET_MINTS[to],
        receivedVault: ata(margin, ASSET_MINTS[to], tokenProgramFor(to)),
        receivedPriceUpdate: prices[to],
        spentTokenProgram: tokenProgramFor(from),
        receivedTokenProgram: tokenProgramFor(to),
        associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .remainingAccounts([...swap.accounts, ...health])
      .instruction();

    // A route plus the health scan exceeds a legacy transaction; Jupiter's lookup tables fit it.
    const tables = await Promise.all(swap.lookupTables.map((t) => conn.getAddressLookupTable(t)));
    const message = new TransactionMessage({
      payerKey: wallet.publicKey,
      recentBlockhash: (await conn.getLatestBlockhash()).blockhash,
      instructions: [ComputeBudgetProgram.setComputeUnitLimit({ units: 1_000_000 }), ix],
    }).compileToV0Message(tables.map((t) => t.value).filter((t): t is AddressLookupTableAccount => t !== null));
    const tx = new VersionedTransaction(message);
    tx.sign([wallet]);
    const sig = await conn.sendTransaction(tx);
    await conn.confirmTransaction(sig, "confirmed");
    log("jupiter-swap", `${args.amount} ${from} -> ${to} (quoted ${swap.quotedOut}) tx=${sig}`);
  },
};

async function refreshAllPrices(conn: Connection, wallet: anchor.web3.Keypair): Promise<Record<AssetKey, PublicKey>> {
  const anchorWallet = new anchor.Wallet(wallet);
  const prices = {} as Record<AssetKey, PublicKey>;
  for (const key of Object.keys(ASSET_MINTS) as AssetKey[]) prices[key] = await refreshPrice(conn, anchorWallet, key);
  return prices;
}

/**
 * Deposits underlying into (or redeems cTokens from) Kamino from the margin account via
 * `margin_execute`. A deposit spends underlying and receives cTokens; a redeem the reverse.
 * `--amount` is in the spent token's units, `--min-received` in the received token's.
 */
async function kaminoCall({ args, wallet, program, conn }: Ctx, call: KaminoCall): Promise<void> {
  const key = receiptKeyFromString(requireArg(args, "symbol"));
  const receipt = KAMINO_RECEIPTS[key];
  const underlying = receipt.underlying;
  const [spentDecimals, receivedDecimals] =
    call === "deposit" ? [ASSET_DECIMALS[underlying], CTOKEN_DECIMALS] : [CTOKEN_DECIMALS, ASSET_DECIMALS[underlying]];
  const amount = BigInt(toBaseUnits(requireArg(args, "amount"), spentDecimals).toString());
  const minReceived = toBaseUnits(optionalArg(args, "min-received", "0"), receivedDecimals);
  const [margin] = marginPda(wallet.publicKey);

  const legs = {
    underlying: { mint: ASSET_MINTS[underlying], tokenProgram: tokenProgramFor(underlying) },
    receipt: { mint: receipt.collateralMint, tokenProgram: TOKEN_PROGRAM_ID },
  };
  const [spent, received] = call === "deposit" ? [legs.underlying, legs.receipt] : [legs.receipt, legs.underlying];

  const prices = await refreshAllPrices(conn, wallet);
  const health = await buildRemainingAccounts(program, margin, prices, { excludeCollateral: [underlying, key] });
  const cpiAccounts = kaminoCallAccounts(call, receipt, margin);
  const [protocolConfig] = protocolConfigPda();
  const [integration] = integrationPda(KLEND);

  const sig = await method(program, "margin_execute", "marginExecute")(
    kaminoCallData(call, amount),
    cpiAccounts.length,
    minReceived,
  )
    .accounts({
      authority: wallet.publicKey,
      protocolConfig,
      marginAccount: margin,
      integration,
      targetProgram: KLEND,
      spentAsset: assetConfigPda(spent.mint)[0],
      spentMint: spent.mint,
      spentVault: ata(margin, spent.mint, spent.tokenProgram),
      spentPriceUpdate: prices[underlying],
      receivedAsset: assetConfigPda(received.mint)[0],
      receivedMint: received.mint,
      receivedVault: ata(margin, received.mint, received.tokenProgram),
      receivedPriceUpdate: prices[underlying],
      spentTokenProgram: spent.tokenProgram,
      receivedTokenProgram: received.tokenProgram,
      associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
      systemProgram: SystemProgram.programId,
    })
    .remainingAccounts([...cpiAccounts, ...health])
    .preInstructions([ComputeBudgetProgram.setComputeUnitLimit({ units: 600_000 })])
    .rpc();
  log(`kamino-${call}`, `${key} amount=${args.amount} tx=${sig}`);
}

async function main() {
  const argv = process.argv.slice(2);
  const command = argv[0];
  if (!command || !commands[command]) {
    console.log("Usage: npx tsx src/integrations-fork.ts <command> [--flag value]");
    console.log("Commands:", Object.keys(commands).join(", "));
    process.exit(command ? 1 : 0);
  }
  const args = parseArgs(argv.slice(1));
  const wallet = loadKeypair(args.wallet);
  const conn = devnetConnection();
  const program = programAs(conn, wallet);
  await commands[command]({ args, conn, wallet, program });
}

main().catch((err) => {
  console.error(err instanceof Error ? err.message : err);
  process.exit(1);
});
