/**
 * Jupiter v6 client for the `Jupiter` adapter (`programs/.../adapters/jupiter.rs`): fetches a
 * route whose user is the margin PDA, and checks it is one the adapter allows.
 */
import { AccountMeta, PublicKey } from "@solana/web3.js";

export const JUPITER = new PublicKey("JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4");
const API = process.env.JUPITER_API_URL ?? "https://lite-api.jup.ag/swap/v1";

/** sha256("global:route")[0..8] and sha256("global:shared_accounts_route")[0..8]. */
const ALLOWED = new Map<string, string>([
  [Buffer.from([229, 23, 203, 151, 122, 227, 173, 42]).toString("hex"), "route"],
  [Buffer.from([193, 32, 155, 51, 65, 214, 156, 129]).toString("hex"), "shared_accounts_route"],
]);

export interface MarginSwap {
  /** Jupiter instruction data, passed through `margin_execute` unchanged. */
  data: Buffer;
  /** Jupiter's accounts; the margin signs inside Vanna, so none is a signer here. */
  accounts: AccountMeta[];
  lookupTables: PublicKey[];
  quotedOut: bigint;
  /** Jupiter's own slippage floor; a sensible `min_received`. */
  minOut: bigint;
}

interface JupiterAccount {
  pubkey: string;
  isSigner: boolean;
  isWritable: boolean;
}

/**
 * An exact-input route for `margin`, with no platform fee and output into the margin's own
 * destination account, as the adapter requires.
 */
export async function jupiterRouteForMargin(opts: {
  inputMint: PublicKey;
  outputMint: PublicKey;
  amount: bigint;
  margin: PublicKey;
  slippageBps: number;
}): Promise<MarginSwap> {
  const params = new URLSearchParams({
    inputMint: opts.inputMint.toBase58(),
    outputMint: opts.outputMint.toBase58(),
    amount: opts.amount.toString(),
    slippageBps: String(opts.slippageBps),
    swapMode: "ExactIn",
  });
  const quoteResponse = await fetchJson(`${API}/quote?${params}`);
  const body = await fetchJson(`${API}/swap-instructions`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      quoteResponse,
      userPublicKey: opts.margin.toBase58(),
      wrapAndUnwrapSol: false,
      useSharedAccounts: true,
    }),
  });

  const ix = body.swapInstruction as { programId: string; accounts: JupiterAccount[]; data: string };
  if (ix.programId !== JUPITER.toBase58()) throw new Error(`Jupiter returned a ${ix.programId} instruction`);
  const data = Buffer.from(ix.data, "base64");
  const kind = ALLOWED.get(data.subarray(0, 8).toString("hex"));
  if (!kind) {
    throw new Error(`Jupiter returned an instruction the adapter refuses (selector ${data.subarray(0, 8).toString("hex")})`);
  }
  return {
    data,
    accounts: ix.accounts.map((a) => ({ pubkey: new PublicKey(a.pubkey), isSigner: false, isWritable: a.isWritable })),
    lookupTables: ((body.addressLookupTableAddresses as string[]) ?? []).map((a) => new PublicKey(a)),
    quotedOut: BigInt(quoteResponse.outAmount),
    minOut: BigInt(quoteResponse.otherAmountThreshold),
  };
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
async function fetchJson(url: string, init?: RequestInit): Promise<any> {
  const response = await fetch(url, init);
  if (!response.ok) throw new Error(`${url}: ${response.status} ${await response.text()}`);
  return response.json();
}
