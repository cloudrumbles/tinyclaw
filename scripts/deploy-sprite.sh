#!/usr/bin/env bash
# =============================================================================
# deploy-sprite.sh — Deploy TinyClaw (Rust) to a Sprite VM
# =============================================================================
#
# Uses the Sprites HTTP API + filesystem API for file transfer (no B2 needed).
#
# Flow:
#   1. Build Rust binary locally
#   2. Upload tarball via Sprites filesystem API
#   3. Extract + install claude CLI on sprite
#   4. Write configs via exec API (base64-encoded to avoid quoting issues)
#   5. Create service via sprite-env with --http-port (wake-on-request)
#   6. Checkpoint for rollback
#
# Prerequisites:
#   - cargo installed locally
#   - .env file with: SPRITES_TOKEN, TELEGRAM_BOT_TOKEN, CLAUDE_CODE_OAUTH_TOKEN
#   - Optional: B2_KEY_ID, B2_APP_KEY (no longer needed for deploy)
#
# Usage:
#   ./scripts/deploy-sprite.sh <sprite-name> [options]
#
# Tokens are read from .env by default. CLI flags override .env values.
# Webhook URL is auto-derived from the sprite's public URL.
#
# Options:
#   --telegram-token <token>   Override TELEGRAM_BOT_TOKEN from .env
#   --claude-token <token>     Override CLAUDE_CODE_OAUTH_TOKEN from .env
#   --webhook-url <url>        Override auto-derived webhook URL
#   --agent-name <name>        Agent display name (default: Assistant)
#   --agent-id <id>            Agent ID (default: assistant)
#   --model <model>            Claude model shortname (default: opus)
#   --pair-telegram <uid:name> Pre-approve a Telegram user
#   --skip-build               Skip cargo build (use existing binary)
#   --skip-claude-install      Skip installing claude CLI on sprite
#
# Example:
#   ./scripts/deploy-sprite.sh sultana \
#       --agent-name "Sultana" \
#       --agent-id sultana \
#       --model opus \
#       --pair-telegram "525365593:Shah"
# =============================================================================

set -euo pipefail

SPRITES_API="https://api.sprites.dev/v1"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

die() { echo -e "${RED}Error: $1${NC}" >&2; exit 1; }
info() { echo -e "${BLUE}→${NC} $1"; }
ok() { echo -e "${GREEN}✓${NC} $1"; }
warn() { echo -e "${YELLOW}!${NC} $1"; }

# ---------------------------------------------------------------------------
# Sprites API helpers
# ---------------------------------------------------------------------------
sprite_run() {
    local script_file
    script_file=$(mktemp /tmp/sprite-cmd-XXXXXX.sh)
    printf '%s' "$1" > "$script_file"
    local response
    response=$(curl -sS -w "\n%{http_code}" -X POST \
        -H "Authorization: Bearer $SPRITES_TOKEN" \
        --data-binary @"$script_file" \
        "${SPRITES_API}/sprites/${SPRITE_NAME}/exec?cmd=bash&stdin=true" | tr -d '\0')
    rm -f "$script_file"
    local http_code body
    http_code=$(echo "$response" | tail -1)
    body=$(echo "$response" | sed '$d')
    if [ "$http_code" -ge 400 ] 2>/dev/null; then
        die "sprite exec failed (HTTP $http_code): $body"
    fi
    echo "$body"
}

sprite_upload() {
    local local_path="$1"
    local remote_path="$2"
    local response
    response=$(curl -sS -w "\n%{http_code}" -X PUT \
        -H "Authorization: Bearer $SPRITES_TOKEN" \
        -H "Content-Type: application/octet-stream" \
        --data-binary @"$local_path" \
        "${SPRITES_API}/sprites/${SPRITE_NAME}/fs/write?path=${remote_path}&mkdir=true")
    local http_code
    http_code=$(echo "$response" | tail -1)
    if [ "$http_code" -ge 400 ] 2>/dev/null; then
        die "fs upload failed (HTTP $http_code): $(echo "$response" | sed '$d')"
    fi
}

# ---------------------------------------------------------------------------
# Path constants
# ---------------------------------------------------------------------------

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

REMOTE_HOME="/home/sprite"
REMOTE_TINYCLAW="$REMOTE_HOME/tinyclaw"
REMOTE_CONFIG="$REMOTE_HOME/.tinyclaw"
REMOTE_WORKSPACE="$REMOTE_HOME/tinyclaw-workspace"

# ---------------------------------------------------------------------------
# Load .env defaults
# ---------------------------------------------------------------------------

if [ -f "$PROJECT_ROOT/.env" ]; then
    set -a
    # shellcheck source=/dev/null
    source "$PROJECT_ROOT/.env"
    set +a
fi

# ---------------------------------------------------------------------------
# Parse arguments
# ---------------------------------------------------------------------------

SPRITE_NAME="${1:-}"
[ -z "$SPRITE_NAME" ] && die "Usage: $0 <sprite-name> [options]"
shift

TELEGRAM_TOKEN="${TELEGRAM_BOT_TOKEN:-}"
CLAUDE_TOKEN="${CLAUDE_CODE_OAUTH_TOKEN:-}"
WEBHOOK_URL=""
AGENT_NAME="Assistant"
AGENT_ID="assistant"
MODEL="opus"
PAIR_TELEGRAM=""
SKIP_BUILD=false
SKIP_CLAUDE_INSTALL=false

while [ $# -gt 0 ]; do
    case "$1" in
        --telegram-token)      TELEGRAM_TOKEN="$2"; shift 2 ;;
        --claude-token)        CLAUDE_TOKEN="$2"; shift 2 ;;
        --webhook-url)         WEBHOOK_URL="$2"; shift 2 ;;
        --agent-name)          AGENT_NAME="$2"; shift 2 ;;
        --agent-id)            AGENT_ID="$2"; shift 2 ;;
        --model)               MODEL="$2"; shift 2 ;;
        --pair-telegram)       PAIR_TELEGRAM="$2"; shift 2 ;;
        --skip-build)          SKIP_BUILD=true; shift ;;
        --skip-claude-install) SKIP_CLAUDE_INSTALL=true; shift ;;
        *) die "Unknown option: $1" ;;
    esac
done

[ -z "$TELEGRAM_TOKEN" ] && die "TELEGRAM_BOT_TOKEN not set in .env and --telegram-token not provided"
[ -z "$CLAUDE_TOKEN" ]   && die "CLAUDE_CODE_OAUTH_TOKEN not set in .env and --claude-token not provided"
: "${SPRITES_TOKEN:?SPRITES_TOKEN env var is required}"

# ---------------------------------------------------------------------------
# Step 1: Build Rust binary
# ---------------------------------------------------------------------------

if [ "$SKIP_BUILD" = true ]; then
    info "Skipping build (--skip-build)"
else
    info "Building Rust binary (release)..."
    cd "$PROJECT_ROOT"
    cargo build --release || die "cargo build failed"
fi

BINARY="$PROJECT_ROOT/target/release/tinyclaw"
[ -f "$BINARY" ] || die "Binary not found at $BINARY"
ok "Binary ready ($(du -h "$BINARY" | cut -f1))"

# ---------------------------------------------------------------------------
# Step 2: Ensure sprite exists + set public URL
# ---------------------------------------------------------------------------

info "Checking sprite '$SPRITE_NAME'..."

SPRITE_INFO=$(curl -sS -w "\n%{http_code}" \
    -H "Authorization: Bearer $SPRITES_TOKEN" \
    "${SPRITES_API}/sprites/${SPRITE_NAME}")
SPRITE_HTTP=$(echo "$SPRITE_INFO" | tail -1)
SPRITE_BODY=$(echo "$SPRITE_INFO" | sed '$d')

if [ "$SPRITE_HTTP" = "404" ]; then
    info "Sprite not found, creating '$SPRITE_NAME'..."
    CREATE_RESULT=$(curl -sS -X POST \
        -H "Authorization: Bearer $SPRITES_TOKEN" \
        -H "Content-Type: application/json" \
        -d "{\"name\":\"$SPRITE_NAME\"}" \
        "${SPRITES_API}/sprites")
    SPRITE_URL=$(echo "$CREATE_RESULT" | python3 -c "import sys,json; print(json.load(sys.stdin)['url'])")
    ok "Sprite created: $SPRITE_URL"
elif [ "$SPRITE_HTTP" -ge 400 ] 2>/dev/null; then
    die "Failed to check sprite (HTTP $SPRITE_HTTP): $SPRITE_BODY"
else
    SPRITE_URL=$(echo "$SPRITE_BODY" | python3 -c "import sys,json; print(json.load(sys.stdin)['url'])")
    ok "Sprite exists: $SPRITE_URL"
fi

# Set URL auth to public (required for Telegram webhooks)
info "Setting URL auth to public..."
curl -sS -X PUT \
    -H "Authorization: Bearer $SPRITES_TOKEN" \
    -H "Content-Type: application/json" \
    -d '{"url_settings":{"auth":"public"}}' \
    "${SPRITES_API}/sprites/${SPRITE_NAME}" > /dev/null
ok "URL auth set to public"

# Auto-derive webhook URL if not provided
if [ -z "$WEBHOOK_URL" ]; then
    WEBHOOK_URL="${SPRITE_URL}/webhook"
fi
ok "Webhook URL: $WEBHOOK_URL"

# ---------------------------------------------------------------------------
# Step 3: Create tarball + upload via filesystem API
# ---------------------------------------------------------------------------

info "Creating deployment tarball..."
STAGING=$(mktemp -d /tmp/tinyclaw-stage-XXXXXX)
mkdir -p "$STAGING/tinyclaw"
cp "$BINARY" "$STAGING/tinyclaw/tinyclaw"

if [ -d "$PROJECT_ROOT/.agents/skills" ]; then
    mkdir -p "$STAGING/tinyclaw/.agents"
    cp -r "$PROJECT_ROOT/.agents/skills" "$STAGING/tinyclaw/.agents/skills"
    ok "Skills included"
fi

TARBALL=$(mktemp /tmp/tinyclaw-deploy-XXXXXX.tar.gz)
tar czf "$TARBALL" -C "$STAGING" tinyclaw
rm -rf "$STAGING"
ok "Tarball created ($(du -h "$TARBALL" | cut -f1))"

info "Uploading tarball to sprite via filesystem API..."
sprite_upload "$TARBALL" "/tmp/tinyclaw-deploy.tar.gz"
rm -f "$TARBALL"
ok "Tarball uploaded"

# Wake sprite + extract tarball
info "Extracting on sprite..."
sprite_run "
set -e
rm -rf ${REMOTE_TINYCLAW}
mkdir -p ${REMOTE_HOME}
cd ${REMOTE_HOME} && tar xzf /tmp/tinyclaw-deploy.tar.gz
rm /tmp/tinyclaw-deploy.tar.gz
chmod +x ${REMOTE_TINYCLAW}/tinyclaw
echo DONE
" > /dev/null
ok "Code deployed to sprite"

# ---------------------------------------------------------------------------
# Step 4: Install claude CLI on sprite
# ---------------------------------------------------------------------------

if [ "$SKIP_CLAUDE_INSTALL" = true ]; then
    info "Skipping claude CLI install (--skip-claude-install)"
else
    info "Checking claude CLI on sprite..."
    CLAUDE_CHECK=$(sprite_run "which claude 2>/dev/null && claude --version 2>/dev/null || echo NOT_FOUND" || true)
    if echo "$CLAUDE_CHECK" | grep -q "NOT_FOUND"; then
        info "Installing claude CLI on sprite..."
        sprite_run "
set -e
curl -fsSL https://cli.anthropic.com/install.sh | sh
echo DONE
" > /dev/null
        ok "Claude CLI installed"
    else
        ok "Claude CLI already installed"
    fi
fi

# ---------------------------------------------------------------------------
# Step 5: Remote setup (dirs, wrapper, configs)
# ---------------------------------------------------------------------------

info "Running remote setup..."

# Dirs + wrapper script
sprite_run "
set -e
mkdir -p ${REMOTE_CONFIG}/{logs,files,chats,cron-inbox}
mkdir -p ${REMOTE_WORKSPACE}/${AGENT_ID}
printf '%s\n' '#!/bin/bash' 'cd /home/sprite/tinyclaw' 'set -a; source .env; set +a' 'exec ./tinyclaw 2>&1' > ${REMOTE_TINYCLAW}/run.sh
chmod +x ${REMOTE_TINYCLAW}/run.sh
echo DONE
" > /dev/null

# Settings (base64 to avoid quoting issues)
SETTINGS_JSON=$(cat <<EOF
{
  "workspace": {"path": "${REMOTE_WORKSPACE}"},
  "agents": {
    "${AGENT_ID}": {
      "name": "${AGENT_NAME}",
      "provider": "anthropic",
      "model": "${MODEL}",
      "working_directory": "${AGENT_ID}"
    }
  },
  "channels": {"telegram": {}},
  "monitoring": {"heartbeat_interval": 7200}
}
EOF
)
SETTINGS_B64=$(printf '%s' "$SETTINGS_JSON" | base64 -w0)
sprite_run "echo '${SETTINGS_B64}' | base64 -d > ${REMOTE_CONFIG}/settings.json" > /dev/null

# Secrets (.env)
ENV_CONTENT="TELEGRAM_BOT_TOKEN=${TELEGRAM_TOKEN}
CLAUDE_CODE_OAUTH_TOKEN=${CLAUDE_TOKEN}
WEBHOOK_URL=${WEBHOOK_URL}"
ENV_B64=$(printf '%s' "$ENV_CONTENT" | base64 -w0)
sprite_run "echo '${ENV_B64}' | base64 -d > ${REMOTE_TINYCLAW}/.env && chmod 600 ${REMOTE_TINYCLAW}/.env" > /dev/null

# Pairing (optional)
if [ -n "$PAIR_TELEGRAM" ]; then
    PAIR_USER_ID="${PAIR_TELEGRAM%%:*}"
    PAIR_DISPLAY="${PAIR_TELEGRAM##*:}"
    PAIRING_JSON=$(cat <<EOF
{
  "pending": [],
  "approved": [
    {
      "channel": "telegram",
      "senderId": "${PAIR_USER_ID}",
      "sender": "${PAIR_DISPLAY}",
      "approvedAt": $(date +%s)000,
      "approvedCode": "DEPLOYED"
    }
  ]
}
EOF
)
    PAIR_B64=$(printf '%s' "$PAIRING_JSON" | base64 -w0)
    sprite_run "echo '${PAIR_B64}' | base64 -d > ${REMOTE_CONFIG}/pairing.json" > /dev/null
    ok "Pairing configured for ${PAIR_DISPLAY}"
fi

ok "Remote setup done"

# ---------------------------------------------------------------------------
# Step 6: Create/restart service via sprite-env on the VM
# ---------------------------------------------------------------------------

info "Setting up tinyclaw service..."
sprite_run "
set -e
sprite-env services stop tinyclaw 2>/dev/null || true
sprite-env services delete tinyclaw 2>/dev/null || true
sprite-env services create tinyclaw --cmd bash --args '${REMOTE_TINYCLAW}/run.sh' --http-port 8080 --no-stream
echo DONE
" > /dev/null
ok "Service created with --http-port 8080 (wake-on-request enabled)"

# ---------------------------------------------------------------------------
# Step 7: Verify
# ---------------------------------------------------------------------------

info "Verifying (waiting 5s for startup)..."
sleep 5

LOGS=$(sprite_run "cat /.sprite/logs/services/tinyclaw.log 2>/dev/null | tail -20" || true)
if echo "$LOGS" | grep -q "Webhook listener started"; then
    ok "TinyClaw is running with webhook on port 8080!"
elif echo "$LOGS" | grep -q "TinyClaw starting"; then
    ok "TinyClaw is running (webhook may still be starting)..."
else
    warn "Service may still be starting — check logs"
fi

# ---------------------------------------------------------------------------
# Step 8: Checkpoint
# ---------------------------------------------------------------------------

info "Creating deployment checkpoint..."
sprite_run "sprite-env checkpoints create --comment 'deploy $(date +%Y%m%d-%H%M%S)' 2>&1" > /dev/null
ok "Checkpoint created"

echo ""
echo -e "${GREEN}=== Deployment complete ===${NC}"
echo ""
echo "  Sprite:      $SPRITE_NAME"
echo "  URL:         $SPRITE_URL"
echo "  Agent:       $AGENT_NAME ($AGENT_ID) [anthropic/$MODEL]"
echo "  Channel:     Telegram (webhook)"
echo "  Webhook:     $WEBHOOK_URL"
echo "  Service:     tinyclaw (--http-port 8080, wake-on-request)"
echo ""
