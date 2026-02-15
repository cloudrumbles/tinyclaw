#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["e2b"]
# ///
# =============================================================================
# deploy-e2b.py — Deploy TinyClaw to E2B + webhook proxy to Sprite
# =============================================================================
#
# Architecture:
#   Telegram → Sprite (wake-on-request) → E2B sandbox (auto-resume)
#
# The Sprite VM runs a lightweight webhook proxy that:
#   1. Wakes on incoming Telegram webhook (Sprite's wake-on-request)
#   2. Resumes the E2B sandbox if paused (~1s via Sandbox.connect)
#   3. Forwards the webhook to tinyclaw running on E2B
#
# Flow:
#   1. Build Rust binary locally
#   2. Create E2B sandbox (or reconnect to existing)
#   3. Deploy tinyclaw binary + configs to E2B
#   4. Deploy webhook proxy to Sprite
#   5. Start services on both
#
# Prerequisites:
#   - cargo installed locally
#   - uv installed locally
#   - .env with: E2B_API_KEY, SPRITES_TOKEN, TELEGRAM_BOT_TOKEN,
#                CLAUDE_CODE_OAUTH_TOKEN
#
# Usage:
#   ./scripts/deploy-e2b.py <sprite-name> [options]
#
# Options:
#   --sandbox-id <id>           Reconnect to existing E2B sandbox
#   --telegram-token <token>    Override TELEGRAM_BOT_TOKEN from .env
#   --claude-token <token>      Override CLAUDE_CODE_OAUTH_TOKEN from .env
#   --agent-name <name>         Agent display name (default: Assistant)
#   --agent-id <id>             Agent ID (default: assistant)
#   --model <model>             Claude model shortname (default: opus)
#   --pair-telegram <uid:name>  Pre-approve a Telegram user
#   --skip-build                Skip cargo build (use existing binary)
#   --skip-claude-install       Skip installing claude CLI on E2B
#   --timeout <seconds>         E2B sandbox timeout (default: 86400 = 24h)
#
# Example:
#   ./scripts/deploy-e2b.py sultana \
#       --agent-name "Sultana" \
#       --agent-id sultana \
#       --model opus \
#       --pair-telegram "525365593:Shah"
# =============================================================================

import argparse
import json
import os
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.request
import urllib.error
from pathlib import Path

# ── Colors ──────────────────────────────────────────────────────────────────

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


# ── .env loader ─────────────────────────────────────────────────────────────


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


# ── Sprites API helpers ─────────────────────────────────────────────────────

SPRITES_API = "https://api.sprites.dev/v1"


def sprite_request(
    method: str,
    path: str,
    sprites_token: str,
    data: bytes | None = None,
    content_type: str = "application/json",
) -> tuple[int, str]:
    """Make an HTTP request to the Sprites API."""
    url = f"{SPRITES_API}{path}"
    req = urllib.request.Request(url, method=method, data=data)
    req.add_header("Authorization", f"Bearer {sprites_token}")
    if data and content_type:
        req.add_header("Content-Type", content_type)
    try:
        with urllib.request.urlopen(req) as resp:
            return resp.status, resp.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()


def sprite_exec(sprite_name: str, sprites_token: str, script: str) -> str:
    """Execute a script on a Sprite via the exec API."""
    with tempfile.NamedTemporaryFile(
        mode="w", suffix=".sh", delete=False
    ) as tmp:
        tmp.write(script)
        tmp_path = tmp.name

    script_bytes = Path(tmp_path).read_bytes()
    os.unlink(tmp_path)

    code, body = sprite_request(
        "POST",
        f"/sprites/{sprite_name}/exec?cmd=bash&stdin=true",
        sprites_token,
        data=script_bytes,
        content_type="application/octet-stream",
    )
    if code >= 400:
        die(f"sprite exec failed (HTTP {code}): {body}")
    return body


def sprite_upload(
    sprite_name: str, sprites_token: str, local_path: str, remote_path: str
) -> None:
    """Upload a file to a Sprite via the filesystem API."""
    data = Path(local_path).read_bytes()
    code, body = sprite_request(
        "PUT",
        f"/sprites/{sprite_name}/fs/write?path={remote_path}&mkdir=true",
        sprites_token,
        data=data,
        content_type="application/octet-stream",
    )
    if code >= 400:
        die(f"sprite upload failed (HTTP {code}): {body}")


def sprite_get_or_create(sprite_name: str, sprites_token: str) -> str:
    """Get the public URL of a Sprite, creating it if it doesn't exist."""
    code, body = sprite_request(
        "GET", f"/sprites/{sprite_name}", sprites_token
    )
    if code == 404:
        info(f"Sprite '{sprite_name}' not found, creating...")
        data = json.dumps({"name": sprite_name}).encode()
        code, body = sprite_request(
            "POST", "/sprites", sprites_token, data=data
        )
        if code >= 400:
            die(f"Failed to create sprite (HTTP {code}): {body}")
        ok(f"Sprite created")
    elif code >= 400:
        die(f"Failed to get sprite info (HTTP {code}): {body}")
    return json.loads(body)["url"]


def sprite_set_public(sprite_name: str, sprites_token: str) -> None:
    """Set Sprite URL auth to public."""
    data = json.dumps({"url_settings": {"auth": "public"}}).encode()
    sprite_request("PUT", f"/sprites/{sprite_name}", sprites_token, data=data)


# ── Args ─────────────────────────────────────────────────────────────────────


def parse_args():
    parser = argparse.ArgumentParser(
        description="Deploy TinyClaw to E2B + webhook proxy to Sprite"
    )
    parser.add_argument("sprite_name", help="Sprite name for webhook proxy")
    parser.add_argument("--sandbox-id", help="Reconnect to existing E2B sandbox")
    parser.add_argument("--telegram-token", help="Override TELEGRAM_BOT_TOKEN")
    parser.add_argument("--claude-token", help="Override CLAUDE_CODE_OAUTH_TOKEN")
    parser.add_argument("--agent-name", default="Assistant")
    parser.add_argument("--agent-id", default="assistant")
    parser.add_argument("--model", default="opus")
    parser.add_argument("--pair-telegram", help="uid:name format")
    parser.add_argument("--skip-build", action="store_true")
    parser.add_argument("--skip-claude-install", action="store_true")
    parser.add_argument(
        "--timeout",
        type=int,
        default=3600,
        help="E2B sandbox timeout in seconds (default: 3600 = 1h, max 24h on pro)",
    )
    return parser.parse_args()


# ── Main ─────────────────────────────────────────────────────────────────────


def main():
    from e2b import Sandbox

    project_root = Path(__file__).resolve().parent.parent
    load_env(project_root)

    args = parse_args()

    # Resolve tokens
    telegram_token = args.telegram_token or os.environ.get("TELEGRAM_BOT_TOKEN")
    claude_token = args.claude_token or os.environ.get("CLAUDE_CODE_OAUTH_TOKEN")
    e2b_api_key = os.environ.get("E2B_API_KEY")
    sprites_token = os.environ.get("SPRITES_TOKEN")

    if not telegram_token:
        die("TELEGRAM_BOT_TOKEN not set")
    if not claude_token:
        die("CLAUDE_CODE_OAUTH_TOKEN not set")
    if not e2b_api_key:
        die("E2B_API_KEY not set")
    if not sprites_token:
        die("SPRITES_TOKEN not set")

    # E2B path constants
    remote_home = "/home/user"
    remote_tinyclaw = f"{remote_home}/tinyclaw"
    remote_config = f"{remote_home}/.tinyclaw"
    remote_workspace = f"{remote_home}/tinyclaw-workspace"

    # Sprite path constants
    sprite_home = "/home/sprite"
    sprite_proxy_dir = f"{sprite_home}/e2b-proxy"

    # =========================================================================
    # Phase 1: Build
    # =========================================================================
    binary = project_root / "target" / "release" / "tinyclaw"
    if not args.skip_build:
        info("Building Rust binary (release)...")
        result = subprocess.run(["cargo", "build", "--release"], cwd=project_root)
        if result.returncode != 0:
            die("cargo build failed")
    if not binary.exists():
        die(f"Binary not found at {binary}")
    binary_size = binary.stat().st_size / (1024 * 1024)
    ok(f"Binary ready ({binary_size:.1f}M)")

    # =========================================================================
    # Phase 2: Set up Sprite (get its public URL first — needed for webhook)
    # =========================================================================
    info(f"Checking sprite '{args.sprite_name}'...")
    sprite_url = sprite_get_or_create(args.sprite_name, sprites_token)
    sprite_set_public(args.sprite_name, sprites_token)
    webhook_url = f"{sprite_url}/webhook"
    ok(f"Sprite URL: {sprite_url}")
    ok(f"Webhook URL: {webhook_url}")

    # =========================================================================
    # Phase 3: Create or reconnect E2B sandbox
    # =========================================================================
    if args.sandbox_id:
        info(f"Reconnecting to E2B sandbox '{args.sandbox_id}'...")
        sandbox = Sandbox.connect(args.sandbox_id, api_key=e2b_api_key)
        ok(f"Reconnected: {sandbox.sandbox_id}")
    else:
        info("Creating new E2B sandbox...")
        sandbox = Sandbox.create(timeout=args.timeout, api_key=e2b_api_key)
        ok(f"Sandbox created: {sandbox.sandbox_id}")

    sandbox_id = sandbox.sandbox_id
    e2b_host = sandbox.get_host(8080)
    e2b_url = f"https://{e2b_host}"
    ok(f"E2B target: {e2b_url}")

    # =========================================================================
    # Phase 4: Deploy tinyclaw to E2B
    # =========================================================================

    # -- Upload binary + skills --
    info("Creating deployment tarball...")
    with tempfile.NamedTemporaryFile(suffix=".tar.gz", delete=False) as tmp:
        tarball_path = tmp.name

    with tarfile.open(tarball_path, "w:gz") as tar:
        tar.add(str(binary), arcname="tinyclaw/tinyclaw")
        skills_dir = project_root / ".agents" / "skills"
        if skills_dir.exists():
            for skill_file in skills_dir.rglob("*"):
                if skill_file.is_file():
                    rel = skill_file.relative_to(skills_dir)
                    tar.add(
                        str(skill_file),
                        arcname=f"tinyclaw/.agents/skills/{rel}",
                    )
            ok("Skills included")

    tarball_size = os.path.getsize(tarball_path) / (1024 * 1024)
    ok(f"Tarball created ({tarball_size:.1f}M)")

    info("Uploading tarball to E2B...")
    sandbox.files.write("/tmp/tinyclaw-deploy.tar.gz", Path(tarball_path).read_bytes())
    os.unlink(tarball_path)
    ok("Tarball uploaded")

    info("Extracting on E2B...")
    sandbox.commands.run(
        f"rm -rf {remote_tinyclaw} && mkdir -p {remote_home} "
        f"&& cd {remote_home} && tar xzf /tmp/tinyclaw-deploy.tar.gz "
        f"&& rm /tmp/tinyclaw-deploy.tar.gz "
        f"&& chmod +x {remote_tinyclaw}/tinyclaw"
    )
    ok("Code deployed to E2B")

    # -- Install claude CLI --
    if not args.skip_claude_install:
        info("Checking claude CLI on E2B...")
        check = sandbox.commands.run(
            "which claude 2>/dev/null && claude --version 2>/dev/null || echo NOT_FOUND"
        )
        if "NOT_FOUND" in (check.stdout or ""):
            info("Installing claude CLI...")
            sandbox.commands.run(
                "curl -fsSL https://cli.anthropic.com/install.sh | sh",
                timeout=120,
            )
            ok("Claude CLI installed")
        else:
            ok("Claude CLI already installed")
    else:
        info("Skipping claude CLI install")

    # -- Write configs --
    info("Writing E2B configs...")

    sandbox.commands.run(
        f"mkdir -p {remote_config}/logs {remote_config}/files "
        f"{remote_config}/chats {remote_config}/cron-inbox "
        f"{remote_workspace}/{args.agent_id}"
    )

    # run.sh
    sandbox.files.write(
        f"{remote_tinyclaw}/run.sh",
        "#!/bin/bash\n"
        f"cd {remote_tinyclaw}\n"
        "set -a; source .env; set +a\n"
        "exec ./tinyclaw 2>&1\n",
    )
    sandbox.commands.run(f"chmod +x {remote_tinyclaw}/run.sh")

    # settings.json
    settings = {
        "workspace": {"path": remote_workspace},
        "agents": {
            args.agent_id: {
                "name": args.agent_name,
                "provider": "anthropic",
                "model": args.model,
                "working_directory": args.agent_id,
            }
        },
        "channels": {"telegram": {}},
        "monitoring": {"heartbeat_interval": 7200},
    }
    sandbox.files.write(
        f"{remote_config}/settings.json", json.dumps(settings, indent=2)
    )

    # .env — webhook URL points at Sprite (the proxy)
    sandbox.files.write(
        f"{remote_tinyclaw}/.env",
        f"TELEGRAM_BOT_TOKEN={telegram_token}\n"
        f"CLAUDE_CODE_OAUTH_TOKEN={claude_token}\n"
        f"WEBHOOK_URL={webhook_url}\n",
    )
    sandbox.commands.run(f"chmod 600 {remote_tinyclaw}/.env")

    # pairing
    if args.pair_telegram:
        uid, _, display = args.pair_telegram.partition(":")
        pairing = {
            "pending": [],
            "approved": [
                {
                    "channel": "telegram",
                    "senderId": uid,
                    "sender": display,
                    "approvedAt": int(time.time() * 1000),
                    "approvedCode": "DEPLOYED",
                }
            ],
        }
        sandbox.files.write(
            f"{remote_config}/pairing.json", json.dumps(pairing, indent=2)
        )
        ok(f"Pairing configured for {display}")

    # -- Start tinyclaw --
    info("Starting tinyclaw on E2B...")
    try:
        sandbox.commands.run("pkill -f tinyclaw 2>/dev/null; exit 0")
    except Exception:
        pass  # no process to kill
    sandbox.commands.run(f"bash {remote_tinyclaw}/run.sh", background=True)
    ok("TinyClaw started")

    # Verify
    info("Verifying E2B (waiting 5s)...")
    time.sleep(5)
    check = sandbox.commands.run("pgrep -f tinyclaw || echo NOT_RUNNING")
    if "NOT_RUNNING" in (check.stdout or ""):
        warn("tinyclaw may not be running on E2B — check logs")
    else:
        ok("TinyClaw running on E2B")

    ok("E2B deployment done")

    # =========================================================================
    # Phase 5: Deploy webhook proxy to Sprite
    # =========================================================================
    info("Deploying webhook proxy to Sprite...")

    # Upload proxy script
    proxy_script = project_root / "scripts" / "e2b-proxy.ts"
    if not proxy_script.exists():
        die(f"Proxy script not found at {proxy_script}")

    with tempfile.NamedTemporaryFile(
        mode="w", suffix=".ts", delete=False
    ) as tmp:
        tmp.write(proxy_script.read_text())
        tmp_path = tmp.name

    sprite_upload(
        args.sprite_name,
        sprites_token,
        tmp_path,
        f"{sprite_proxy_dir}/proxy.ts",
    )
    os.unlink(tmp_path)
    ok("Proxy script uploaded")

    # Write proxy .env
    proxy_env = (
        f"E2B_API_KEY={e2b_api_key}\n" f"E2B_SANDBOX_ID={sandbox_id}\n"
    )
    import base64

    proxy_env_b64 = base64.b64encode(proxy_env.encode()).decode()
    sprite_exec(
        args.sprite_name,
        sprites_token,
        f"echo '{proxy_env_b64}' | base64 -d > {sprite_proxy_dir}/.env "
        f"&& chmod 600 {sprite_proxy_dir}/.env",
    )
    ok("Proxy .env written")

    # Install e2b npm package + create run script
    info("Installing e2b package on Sprite...")
    sprite_exec(
        args.sprite_name,
        sprites_token,
        f"cd {sprite_proxy_dir} && bun add e2b 2>&1 | tail -3",
    )
    ok("e2b package installed")

    # Write proxy run script
    run_proxy = (
        "#!/bin/bash\n"
        f"cd {sprite_proxy_dir}\n"
        "set -a; source .env; set +a\n"
        f"exec bun run {sprite_proxy_dir}/proxy.ts 2>&1\n"
    )
    run_proxy_b64 = base64.b64encode(run_proxy.encode()).decode()
    sprite_exec(
        args.sprite_name,
        sprites_token,
        f"echo '{run_proxy_b64}' | base64 -d > {sprite_proxy_dir}/run.sh "
        f"&& chmod +x {sprite_proxy_dir}/run.sh",
    )

    # Create/restart Sprite service (stop old tinyclaw service if it exists)
    info("Setting up Sprite proxy service...")
    sprite_exec(
        args.sprite_name,
        sprites_token,
        "set -e\n"
        "sprite-env services stop tinyclaw 2>/dev/null || true\n"
        "sprite-env services delete tinyclaw 2>/dev/null || true\n"
        "sprite-env services stop e2b-proxy 2>/dev/null || true\n"
        "sprite-env services delete e2b-proxy 2>/dev/null || true\n"
        f"sprite-env services create e2b-proxy "
        f"--cmd bash --args '{sprite_proxy_dir}/run.sh' "
        f"--http-port 8080 --no-stream\n"
        "echo DONE",
    )
    ok("Sprite proxy service created (wake-on-request on port 8080)")

    # =========================================================================
    # Summary
    # =========================================================================
    print()
    print(f"{GREEN}=== Deployment complete ==={NC}")
    print()
    print(f"  Sprite:      {args.sprite_name} (webhook proxy)")
    print(f"  Sprite URL:  {sprite_url}")
    print(f"  Webhook:     {webhook_url}")
    print()
    print(f"  E2B sandbox: {sandbox_id}")
    print(f"  E2B target:  {e2b_url}")
    print(f"  E2B timeout: {args.timeout}s ({args.timeout // 3600}h)")
    print()
    print(f"  Agent:       {args.agent_name} ({args.agent_id}) [anthropic/{args.model}]")
    print()
    print(f"  Flow: Telegram → Sprite (wake) → E2B (resume) → tinyclaw")
    print()
    print(f"  Redeploy:    ./scripts/deploy-e2b.py {args.sprite_name} "
          f"--sandbox-id {sandbox_id} --skip-build")
    print()


if __name__ == "__main__":
    main()
