#!/usr/bin/env node
/**
 * Surfpool fork helpers: funding wallets with the protocol's assets, plus the external
 * integrations that go through `margin_execute`:
 *
 *   Funding: `fund --asset X --to PUBKEY --amount N` (any listed asset), `fund-sol` (native SOL)
 *   Kamino:  `register-kamino`, `register-receipt --symbol USDC|SOL`, then
 *            `kamino-deposit` / `kamino-redeem --symbol X --amount N`
 *   Jupiter: `register-jupiter`, then `jupiter-swap --from usdc --to wsol --amount N`
 *
 *   npx tsx src/integrations-fork.ts <command> [--flag value ...]
 */
import * as anchor from "@coral-xyz/anchor";
import { ASSOCIATED_TOKEN_PROGRAM_ID, TOKEN_PROGRAM_ID } from "@solana/spl-token";
import {
  AddressLookupTableAccount,
  ComputeBudgetProgram,
  Connection,
  PublicKey,
  SystemProgram,
  TransactionMessage,
  VersionedTransaction,
} from "@solana/web3.js";
import { ata, optionalArg, parseArgs, requireArg, toBaseUnits } from "./devnet-cli";
import {
  ASSET_DECIMALS,
  ASSET_MINTS,
  AssetKey,
  DEVNET_RPC_URL,
  assetKeyFromString,
  devnetConnection,
  loadKeypair,
  log,
  oracleAccountsFor,
  oracleConfigArg,
  programAs,
  tokenProgramFor,
} from "./devnet-env";
import { buildRemainingAccounts } from "./devnet-positions";
import { JUPITER, jupiterRouteForMargin } from "./jupiter";
import { oracleMetas, refreshOraclesOnFork } from "./oracle";
import {
  CTOKEN_DECIMALS,
  KAMINO_RECEIPTS,
  KaminoCall,
  kaminoCallAccounts,
  kaminoCallData,
  KLEND,
  receiptKeyFromString,
} from "./kamino";
import { assetConfigPda, integrationPda, marginPda, protocolConfigPda } from "./pda";

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

const commands: Record<string, (ctx: Ctx) => Promise<void>> = {
  // -- fork funding (Surfpool cheatcodes / airdrop) -----------------------------------------------
  "fund-sol": async ({ args, conn }) => {
    const to = new PublicKey(requireArg(args, "to"));
    const amount = Number(optionalArg(args, "amount", "10"));
    const sig = await conn.requestAirdrop(to, amount * 1e9);
    await conn.confirmTransaction(sig, "confirmed");
    log("fund-sol", `${amount} SOL → ${to.toBase58()} tx=${sig}`);
  },

  /** Sets `--to`'s token balance of any listed asset (USDC, USDT, JitoSOL, JupSOL, JupUSD, NVDAx,
   * TSLAx…) with the `surfnet_setTokenAccount` cheatcode. Use `fund-sol` for native SOL. */
  fund: async ({ args }) => {
    const asset = assetKeyFromString(requireArg(args, "asset"));
    const to = new PublicKey(requireArg(args, "to"));
    const amount = requireArg(args, "amount");
    const raw = toBaseUnits(amount, ASSET_DECIMALS[asset]);
    await callCheatcode("surfnet_setTokenAccount", [
      to.toBase58(),
      ASSET_MINTS[asset].toBase58(),
      { amount: Number(raw.toString()) },
      tokenProgramFor(asset).toBase58(),
    ]);
    log("fund", `${amount} ${asset} → ${to.toBase58()}`);
  },

  "fund-usdc": async (ctx) => commands.fund({ ...ctx, args: { asset: "usdc", amount: "1000", ...ctx.args } }),

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
   * Registers a Kamino cToken as collateral priced as its underlying (same oracle) through its
   * reserve's exchange rate. The program checks the reserve, its cToken mint, and that the
   * underlying's registered oracle is the one given, so the underlying's `AssetConfig` is passed.
   */
  "register-receipt": async ({ args, wallet, program, conn }) => {
    const key = receiptKeyFromString(requireArg(args, "symbol"));
    const receipt = KAMINO_RECEIPTS[key];
    const [protocolConfig] = protocolConfigPda();
    const [assetConfig] = assetConfigPda(receipt.collateralMint);
    const risk = [
      Number(optionalArg(args, "ltv-bps", "5000")),
      Number(optionalArg(args, "liq-threshold-bps", "6000")),
      Number(optionalArg(args, "liq-bonus-bps", "500")),
    ];
    const maxCollateral = toBaseUnits(optionalArg(args, "max-collateral", "0"), CTOKEN_DECIMALS);
    await refreshOraclesOnFork(conn, new anchor.Wallet(wallet), [receipt.underlying]);
    const oracle = oracleConfigArg(receipt.underlying, { reserve: receipt.reserve, program: KLEND });
    const accounts = [...oracleAccountsFor(receipt.underlying), receipt.reserve, assetConfigPda(ASSET_MINTS[receipt.underlying])[0]];
    const sig = await method(program, "admin_register_asset", "adminRegisterAsset")(oracle, maxCollateral, ...risk, true, false)
      .accounts({
        admin: wallet.publicKey,
        payer: wallet.publicKey,
        protocolConfig,
        underlyingMint: receipt.collateralMint,
        assetConfig,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .remainingAccounts(oracleMetas(accounts))
      .rpc();
    log("register-receipt", `${key} assetConfig=${assetConfig.toBase58()} tx=${sig}`);
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

    await refreshAllOracles(conn, wallet);
    const health = await buildRemainingAccounts(program, margin, { excludeCollateral: [from, to], priced: [from, to] });
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
        receivedAsset: assetConfigPda(ASSET_MINTS[to])[0],
        receivedMint: ASSET_MINTS[to],
        receivedVault: ata(margin, ASSET_MINTS[to], tokenProgramFor(to)),
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

/** Makes every asset's price fresh on the fork before a health-checked call. */
async function refreshAllOracles(conn: Connection, wallet: anchor.web3.Keypair): Promise<void> {
  await refreshOraclesOnFork(conn, new anchor.Wallet(wallet), Object.keys(ASSET_MINTS) as AssetKey[]);
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

  await refreshAllOracles(conn, wallet);
  // The receipt's reserve is in the CPI accounts, read after the call.
  const health = await buildRemainingAccounts(program, margin, { excludeCollateral: [underlying, key], priced: [underlying] });
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
      receivedAsset: assetConfigPda(received.mint)[0],
      receivedMint: received.mint,
      receivedVault: ata(margin, received.mint, received.tokenProgram),
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
