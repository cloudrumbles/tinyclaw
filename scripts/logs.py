#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["blaxel"]
# ///
# =============================================================================
# logs.py — Tail tinyclaw logs from Blaxel sandbox
# =============================================================================
#
# Usage:
#   ./scripts/logs.py              # last 50 lines
#   ./scripts/logs.py -n 200       # last 200 lines
#   ./scripts/logs.py -f           # follow (poll every 2s)
#   ./scripts/logs.py --ps         # show running processes instead
# =============================================================================

import argparse
import asyncio
import os
import sys
from pathlib import Path

LOG_PATH = "/home/tinyclaw/sultana-workspace/logs/tinyclaw.log"


def load_env():
    env_file = Path(__file__).resolve().parent.parent / ".env"
    if not env_file.exists():
        return
    for line in env_file.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        key, _, value = line.partition("=")
        if key and value:
            os.environ.setdefault(key.strip(), value.strip())


async def get_sandbox(name="tinyclaw"):
    from blaxel.core import SandboxInstance

    for s in await SandboxInstance.list():
        if s.metadata.name == name:
            return s
    print(f"sandbox '{name}' not found", file=sys.stderr)
    sys.exit(1)


async def run_cmd(sandbox, cmd):
    result = await sandbox.process.exec({"command": cmd})
    await asyncio.sleep(2)
    proc = await sandbox.process.get(result.name)
    return proc.stdout or ""


async def main():
    parser = argparse.ArgumentParser(description="Tail tinyclaw logs")
    parser.add_argument("-n", "--lines", type=int, default=50)
    parser.add_argument("-f", "--follow", action="store_true")
    parser.add_argument("--ps", action="store_true", help="Show running processes")
    parser.add_argument(
        "--cmd", type=str, help="Run arbitrary command on sandbox"
    )
    parser.add_argument(
        "--dev", action="store_true", help="Use tinyclaw-dev sandbox"
    )
    args = parser.parse_args()

    load_env()
    os.environ["BL_API_KEY"] = os.environ.get("BLAXEL_API_KEY", "")
    os.environ["BL_WORKSPACE"] = os.environ.get("BLAXEL_WORKSPACE", "")

    sandbox_name = "tinyclaw-dev" if args.dev else "tinyclaw"
    sandbox = await get_sandbox(sandbox_name)

    if args.cmd:
        print(await run_cmd(sandbox, args.cmd))
        return

    if args.ps:
        print(await run_cmd(sandbox, "ps aux | grep -v grep | grep -E 'tinyclaw|claude'"))
        return

    if args.follow:
        seen = 0
        while True:
            output = await run_cmd(sandbox, f"wc -l < {LOG_PATH}")
            total = int(output.strip() or "0")
            if total > seen:
                new_lines = total - seen
                output = await run_cmd(sandbox, f"tail -n {new_lines} {LOG_PATH}")
                if output.strip():
                    print(output, end="", flush=True)
                seen = total
            await asyncio.sleep(2)
    else:
        output = await run_cmd(sandbox, f"tail -n {args.lines} {LOG_PATH}")
        print(output, end="")


if __name__ == "__main__":
    asyncio.run(main())
