#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["blaxel"]
# ///
# =============================================================================
# deploy-blaxel.py — Deploy TinyClaw to Blaxel sandbox
# =============================================================================
#
# The binary is self-configuring: tokens, agent config, and pairing are baked
# in at compile time (build.rs reads .env). Only WEBHOOK_URL and WEBHOOK_PORT
# need to be set at runtime (they depend on the deployment target).
#
# Flow:
#   1. Build binary (secrets baked in from .env)
#   2. Create/reconnect Blaxel sandbox
#   3. Upload binary + skills + claude CLI
#   4. Create preview URL → derive WEBHOOK_URL
#   5. Start tinyclaw as non-root user
#
# Usage:
#   ./scripts/deploy-blaxel.py [--skip-build] [--skip-claude-install]
# =============================================================================

import argparse
import asyncio
import os
import shutil
import subprocess
import sys
from pathlib import Path

RED = "\033[0;31m"
GREEN = "\033[0;32m"
YELLOW = "\033[1;33m"
BLUE = "\033[0;34m"
NC = "\033[0m"


def die(msg):
    print(f"{RED}Error: {msg}{NC}", file=sys.stderr)
    sys.exit(1)


def info(msg):
    print(f"{BLUE}→{NC} {msg}")


def ok(msg):
    print(f"{GREEN}✓{NC} {msg}")


def warn(msg):
    print(f"{YELLOW}!{NC} {msg}")


def load_env(project_root: Path) -> None:
    env_file = project_root / ".env"
    if not env_file.exists():
        return
    for line in env_file.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        key, _, value = line.partition("=")
        if key and value:
            os.environ.setdefault(key.strip(), value.strip())


def parse_args():
    parser = argparse.ArgumentParser(description="Deploy TinyClaw to Blaxel sandbox")
    parser.add_argument("--sandbox-name", default="tinyclaw")
    parser.add_argument("--skip-build", action="store_true")
    parser.add_argument("--skip-claude-install", action="store_true")
    parser.add_argument("--region", default="us-pdx-1")
    parser.add_argument("--memory", type=int, default=4096)
    return parser.parse_args()


async def main():
    args = parse_args()

    project_root = Path(__file__).resolve().parent.parent
    load_env(project_root)

    # Set Blaxel env vars BEFORE importing SDK (reads them at import time)
    blaxel_api_key = os.environ.get("BLAXEL_API_KEY")
    blaxel_workspace = os.environ.get("BLAXEL_WORKSPACE")
    if not blaxel_api_key:
        die("BLAXEL_API_KEY not set in .env")
    if not blaxel_workspace:
        die("BLAXEL_WORKSPACE not set in .env")
    os.environ["BL_API_KEY"] = blaxel_api_key
    os.environ["BL_WORKSPACE"] = blaxel_workspace

    from blaxel.core import SandboxInstance

    # Paths on the sandbox
    remote_dir = "/home/user/tinyclaw"
    webhook_port = 3000

    # ── Build ─────────────────────────────────────────────────────────────
    binary = project_root / "target" / "release" / "tinyclaw"
    if not args.skip_build:
        info("Building (secrets baked in from .env)...")
        result = subprocess.run(["cargo", "build", "--release"], cwd=project_root)
        if result.returncode != 0:
            die("cargo build failed")
    if not binary.exists():
        die(f"Binary not found at {binary}")
    binary_size = binary.stat().st_size / (1024 * 1024)
    ok(f"Binary ready ({binary_size:.1f}M)")

    # ── Sandbox ───────────────────────────────────────────────────────────
    info(f"Connecting to sandbox '{args.sandbox_name}'...")
    sandbox = None
    try:
        for s in await SandboxInstance.list():
            if s.metadata.name == args.sandbox_name:
                sandbox = s
                ok(f"Reconnected: {sandbox.process.external_url}")
                break
    except Exception:
        pass

    if sandbox is None:
        sandbox = await SandboxInstance.create({
            "name": args.sandbox_name,
            "image": "sandbox/tinyclaw-sandbox:lx629q5f7p8g",
            "memory": args.memory,
            "ports": [{"target": webhook_port, "protocol": "HTTP"}],
            "region": args.region,
        })
        ok(f"Created: {sandbox.process.external_url}")

    # ── Preview URL ───────────────────────────────────────────────────────
    info("Creating preview URL...")
    preview = await sandbox.previews.create_if_not_exists({
        "metadata": {"name": "webhook"},
        "spec": {"port": webhook_port, "public": True},
    })
    preview_url = preview.spec.url
    webhook_url = f"{preview_url}/webhook"
    ok(f"Webhook: {webhook_url}")

    # ── Stop running process (must happen before upload to avoid "text file busy") ──
    info("Stopping tinyclaw...")
    await sandbox.process.exec({"command": "pkill -f 'tinyclaw' 2>/dev/null; exit 0"})
    await asyncio.sleep(2)
    ok("Stopped")

    # ── Upload binary ─────────────────────────────────────────────────────
    info("Uploading binary...")
    await sandbox.fs.write_binary(f"{remote_dir}/tinyclaw", str(binary))
    await sandbox.process.exec({"command": f"chmod +x {remote_dir}/tinyclaw"})
    ok(f"Binary uploaded ({binary_size:.1f}M)")

    # ── Upload skills ─────────────────────────────────────────────────────
    skills_dir = project_root / ".agents" / "skills"
    if skills_dir.exists():
        info("Uploading skills...")
        for skill_file in skills_dir.rglob("*"):
            if skill_file.is_file():
                rel = skill_file.relative_to(skills_dir)
                await sandbox.fs.write(
                    f"{remote_dir}/.agents/skills/{rel}",
                    skill_file.read_text(),
                )
        ok("Skills uploaded")

    # ── Claude CLI ────────────────────────────────────────────────────────
    if not args.skip_claude_install:
        info("Checking claude CLI...")
        result = await sandbox.process.exec({
            "command": "which claude 2>/dev/null && claude --version 2>/dev/null || echo NOT_FOUND"
        })
        await asyncio.sleep(2)
        proc_info = await sandbox.process.get(result.name)
        if "NOT_FOUND" in (proc_info.stdout or ""):
            info("Uploading claude CLI...")
            claude_bin = shutil.which("claude")
            if not claude_bin:
                die("claude CLI not found locally")
            await sandbox.fs.write_binary(
                "/usr/local/bin/claude", str(Path(claude_bin).resolve())
            )
            await sandbox.process.exec({"command": "chmod +x /usr/local/bin/claude"})
            ok("Claude CLI uploaded")
        else:
            ok("Claude CLI present")

    # ── Non-root user (claude CLI refuses --dangerously-skip-permissions as root) ──
    await sandbox.process.exec({
        "command": "id tinyclaw 2>/dev/null || useradd -m -s /bin/bash tinyclaw"
    })

    # ── Start ─────────────────────────────────────────────────────────────
    info("Starting tinyclaw...")

    # chown everything to the non-root user
    await sandbox.process.exec({
        "command": (
            f"chown -R tinyclaw:tinyclaw {remote_dir} "
            f"/home/user/.tinyclaw /home/user/tinyclaw-workspace 2>/dev/null; "
            f"mkdir -p /home/user/.tinyclaw /home/user/tinyclaw-workspace && "
            f"chown -R tinyclaw:tinyclaw /home/user/.tinyclaw /home/user/tinyclaw-workspace"
        )
    })

    # Start as non-root. Only runtime env vars needed: WEBHOOK_URL + WEBHOOK_PORT
    await sandbox.process.exec({
        "command": (
            f"su - tinyclaw -c '"
            f"cd {remote_dir} && "
            f"WEBHOOK_URL={webhook_url} WEBHOOK_PORT={webhook_port} "
            f"exec ./tinyclaw 2>&1"
            f"'"
        ),
    })
    ok("Started")

    # Verify
    info("Verifying (5s)...")
    await asyncio.sleep(5)
    result = await sandbox.process.exec({
        "command": "pgrep -af tinyclaw | grep -v pgrep || echo NOT_RUNNING"
    })
    await asyncio.sleep(1)
    proc_info = await sandbox.process.get(result.name)
    if "NOT_RUNNING" in (proc_info.stdout or ""):
        warn("tinyclaw may not be running — check logs")
        # Try to get logs
        result = await sandbox.process.exec({
            "command": "cat /home/user/.tinyclaw/logs/tinyclaw.log 2>/dev/null | tail -20"
        })
        await asyncio.sleep(2)
        proc_info = await sandbox.process.get(result.name)
        if proc_info.stdout:
            print(proc_info.stdout)
    else:
        ok("Running")

    # ── Summary ───────────────────────────────────────────────────────────
    print()
    print(f"{GREEN}=== Deployed ==={NC}")
    print(f"  Sandbox:  {args.sandbox_name} ({args.memory}MB, {args.region})")
    print(f"  Webhook:  {webhook_url}")
    print(f"  Redeploy: ./scripts/deploy-blaxel.py --skip-build --skip-claude-install")
    print()


if __name__ == "__main__":
    asyncio.run(main())
