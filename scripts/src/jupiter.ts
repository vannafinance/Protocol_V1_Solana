import { AccountMeta, PublicKey } from "@solana/web3.js";

export const JUPITER = new PublicKey("JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4");
const API = process.env.JUPITER_API_URL ?? "https://lite-api.jup.ag/swap/v1";

const ALLOWED = new Map<string, string>([
  [Buffer.from([229, 23, 203, 151, 122, 227, 173, 42]).toString("hex"), "route"],
  [Buffer.from([193, 32, 155, 51, 65, 214, 156, 129]).toString("hex"), "shared_accounts_route"],
]);

export interface MarginSwap {
  data: Buffer;
  accounts: AccountMeta[];
  lookupTables: PublicKey[];
  quotedOut: bigint;
  minOut: bigint;
}

interface JupiterAccount {
  pubkey: string;
  isSigner: boolean;
  isWritable: boolean;
}

export async function jupiterRouteForMargin(opts: {
  inputMint: PublicKey;
  outputMint: PublicKey;
  amount: bigint;
  margin: PublicKey;
  slippageBps: number;
  dexes?: string;
}): Promise<MarginSwap> {
  const params = new URLSearchParams({
    inputMint: opts.inputMint.toBase58(),
    outputMint: opts.outputMint.toBase58(),
    amount: opts.amount.toString(),
    slippageBps: String(opts.slippageBps),
    swapMode: "ExactIn",
    ...(opts.dexes ? { dexes: opts.dexes } : {}),
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
    throw new Error(`Jupiter returned an instruction the validator refuses (selector ${data.subarray(0, 8).toString("hex")})`);
  }
  return {
    data,
    accounts: ix.accounts.map((a) => ({ pubkey: new PublicKey(a.pubkey), isSigner: false, isWritable: a.isWritable })),
    lookupTables: ((body.addressLookupTableAddresses as string[]) ?? []).map((a) => new PublicKey(a)),
    quotedOut: BigInt(quoteResponse.outAmount),
    minOut: BigInt(quoteResponse.otherAmountThreshold),
  };
}

async function fetchJson(url: string, init?: RequestInit): Promise<any> {
  const response = await fetch(url, init);
  if (!response.ok) throw new Error(`${url}: ${response.status} ${await response.text()}`);
  return response.json();
}
