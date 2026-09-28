#!/usr/bin/env python3
"""Regenerates the mainnet fixtures the external-integration tests load into LiteSVM
(programs/vanna_lending/src/tests/fixtures/mainnet/):

- programs, trimmed to their ELF size and gzipped: Kamino klend, Jupiter v6, Orca Whirlpool;
- accounts, as `owner (32) | lamports (u64 LE) | executable (u8) | data`: klend's main-market
  USDC and SOL reserves, and the Orca SOL/USDC pool Jupiter routes through in the swap tests.

    python3 scripts/dump-mainnet-fixtures.py [--rpc https://api.mainnet-beta.solana.com]

Requires the `solana` CLI (for `solana program dump`). Re-run after an upgrade of any of these
programs, then `anchor build && cargo test`. Tests read pool prices and rates from the fixtures,
and run at the snapshot's block time (`snapshot.rs`), so a refresh needs no test changes.
"""
import argparse
import base64
import gzip
import json
import os
import struct
import subprocess
import tempfile
import time
import urllib.request

PROGRAMS = {
    "klend": "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD",
    "jupiter": "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4",
    "whirlpool": "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc",
}
ACCOUNTS = [
    # Kamino klend main market
    "7u3HeHxYDLhnCoErrtycNokbQYbWGzLs6JSDqGAv5PfF",  # market
    "9DrvZvyWh1HuAoZxvYWMvkf2XCzryCpGgHqrMjyDWpmo",  # market authority
    "D6q6wuQSrifJKZYpR1M8R4YawnLDtDsMmWM1NbBmgJ59",  # USDC reserve
    "Bgq7trRgVMeq33yt235zM2onQ4bRDBsY5EWiTetF4qw6",  # USDC supply vault
    "B8V6WVjPxW1UGwVDfxH2d2r8SyT4cqn7dQRK6XneVa7D",  # cUSDC mint
    "d4A2prbA2whesmvHaL88BH6Ewn5N4bTSU2Ze8P6Bc4Q",  # SOL reserve
    "GafNuUXj9rxGLn4y79dPu6MHSuPWeJR6UtTWuexpGh3U",  # SOL supply vault
    "2UywZrUdyqs5vDchy7fKQJKau2RVyuzBev2XKGPDSiX1",  # cSOL mint
    # Mints
    "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",  # USDC
    "So11111111111111111111111111111111111111112",  # WSOL
    # Orca SOL/USDC whirlpool (tick spacing 4)
    "Czfq3xZZDmsdGdUyrNLtRhGc47cXcZtLG4crryfu44zE",  # pool
    "EUuUbDcafPrmVTD5M6qoJAoyyNbihBhugADAxRMn5he9",  # SOL vault
    "2WLWEuKDgkDUccTpbwYp1GToYktiSB1cXvreHUwiSUVP",  # USDC vault
    "FdtvWk8j5u1a64YK2Uxk9eXxKZJTwLHDGx8aJPbJyw2Q",  # tick array (current)
    "6hA1LN1fzCiXqymDiQXeBFn5da1b7STP1L7JmDc6hR3M",  # tick array (current - 1)
    "D3461zSTVPNdBFPRk2b6zpqQ93g2LW5Kw2potgMdxNJP",  # tick array (current - 2)
    # Jupiter shared-accounts route (program authority id 0 and its token accounts)
    "GGztQqQ6pCPaJQnNpXBgELr5cs3WwDakRbh1iEMzjgSJ",  # program authority
    "g7dD1FHSemkUQrX1Eak37wzvDjscgBW2pFCENwjLdMX",  # its WSOL account
    "DVCeozFGbe6ew3eWTnZByjHeYqTq1cvbrB7JJhkLxaRJ",  # its USDC account
]
OUT = os.path.join(os.path.dirname(__file__), "..", "programs", "vanna_lending", "src", "tests", "fixtures", "mainnet")
B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def b58decode(s: str) -> bytes:
    n = 0
    for c in s:
        n = n * 58 + B58.index(c)
    raw = n.to_bytes((n.bit_length() + 7) // 8, "big")
    return b"\0" * (len(s) - len(s.lstrip("1"))) + raw


def trim_elf(elf: bytes) -> bytes:
    """Drops the program account's zero padding: the ELF ends at its last section's data."""
    assert elf[:4] == b"\x7fELF", "not an ELF"
    (shoff,) = struct.unpack_from("<Q", elf, 0x28)
    shentsize, shnum = struct.unpack_from("<HH", elf, 0x3A)
    end = shoff + shentsize * shnum
    for i in range(shnum):
        off = shoff + i * shentsize
        (sh_type,) = struct.unpack_from("<I", elf, off + 4)
        sh_offset, sh_size = struct.unpack_from("<QQ", elf, off + 0x18)
        if sh_type != 8:  # SHT_NOBITS occupies no file space
            end = max(end, sh_offset + sh_size)
    return elf[:end]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rpc", default="https://api.mainnet-beta.solana.com")
    rpc = parser.parse_args().rpc
    os.makedirs(OUT, exist_ok=True)

    with tempfile.TemporaryDirectory() as tmp:
        for name, program_id in PROGRAMS.items():
            dumped = os.path.join(tmp, f"{name}.so")
            subprocess.run(["solana", "program", "dump", "-u", rpc, program_id, dumped], check=True, capture_output=True)
            with open(dumped, "rb") as f:
                elf = trim_elf(f.read())
            with open(os.path.join(OUT, f"{name}.so.gz"), "wb") as f:
                f.write(gzip.compress(elf, 9, mtime=0))
            print(f"{name}.so.gz: {len(elf)} bytes of ELF")

    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "getMultipleAccounts", "params": [ACCOUNTS, {"encoding": "base64"}]})
    request = urllib.request.Request(rpc, body.encode(), {"Content-Type": "application/json"})
    result = json.load(urllib.request.urlopen(request))["result"]
    for key, account in zip(ACCOUNTS, result["value"]):
        if account is None:
            raise SystemExit(f"{key} not found")
        data = base64.b64decode(account["data"][0])
        blob = b58decode(account["owner"]) + struct.pack("<Q", account["lamports"]) + bytes([account["executable"]]) + data
        with open(os.path.join(OUT, f"{key}.acct"), "wb") as f:
            f.write(blob)
    slot = result["context"]["slot"]
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "getBlockTime", "params": [slot]})
    block_time = json.load(urllib.request.urlopen(urllib.request.Request(rpc, body.encode(), {"Content-Type": "application/json"})))["result"]
    with open(os.path.join(OUT, "snapshot.rs"), "w") as f:
        f.write("// Generated by scripts/dump-mainnet-fixtures.py: block time of the slot the accounts were read at.\n")
        f.write(f"pub const SNAPSHOT_SLOT: u64 = {slot};\n")
        f.write(f"pub const SNAPSHOT_UNIX_TIMESTAMP: i64 = {block_time or int(time.time())};\n")
    print(f"{len(ACCOUNTS)} accounts at slot {slot} (block time {block_time})")


if __name__ == "__main__":
    main()
