#!/usr/bin/env bash
# =============================================================================
# deploy-sprite.sh — Deploy TinyClaw (Rust) to a Sprite VM
# =============================================================================
#
# Uses the Sprites HTTP API + Backblaze B2 for file transfer.
#
# Flow:
#   1. Build Rust binary locally
#   2. Upload tarball to B2, sprite downloads it (exec API can't handle binary)
#   3. Write configs via exec API (base64-encoded to avoid quoting issues)
#   4. Create service via sprite-env on the VM
#
# Prerequisites:
#   - cargo installed locally
#   - SPRITES_TOKEN env var (API token from sprites.dev)
#   - B2_KEY_ID + B2_APP_KEY env vars (Backblaze B2 credentials)
#   - A Sprite VM already created
#
# Usage:
#   ./scripts/deploy-sprite.sh <sprite-name> \
#       --telegram-token <token> \
#       --claude-token <token> \
#       --webhook-url <url> \
#       [--agent-name <name>]   \
#       [--agent-id <id>]       \
#       [--model <model>]       \
#       [--pair-telegram <user_id:display_name>]
#
# Example:
#   ./scripts/deploy-sprite.sh sultana \
#       --telegram-token "6353795033:AAF..." \
#       --claude-token "sk-ant-oat01-..." \
#       --webhook-url "https://sultana-bmewf.sprites.app/webhook" \
#       --agent-name "Sultana" \
#       --agent-id sultana \
#       --model opus \
#       --pair-telegram "525365593:Shah"
# =============================================================================

set -euo pipefail

SPRITES_API="https://api.sprites.dev/v1"
B2_API="https://api.backblazeb2.com"

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
# Sprites API helper — run a script on the sprite via stdin to bash
# ---------------------------------------------------------------------------
sprite_run() {
    local script_file
    script_file=$(mktemp /tmp/sprite-cmd-XXXXXX.sh)
    printf '%s' "$1" > "$script_file"
    local response
    response=$(curl -sS -w "\n%{http_code}" -X POST \
        -H "Authorization: Bearer $SPRITES_TOKEN" \
        --data-binary @"$script_file" \
        "${SPRITES_API}/sprites/${SPRITE_NAME}/exec?cmd=bash&stdin=true")
    rm -f "$script_file"
    local http_code body
    http_code=$(echo "$response" | tail -1)
    body=$(echo "$response" | sed '$d')
    if [ "$http_code" -ge 400 ] 2>/dev/null; then
        die "sprite exec failed (HTTP $http_code): $body"
    fi
    echo "$body"
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
# Parse arguments
# ---------------------------------------------------------------------------

SPRITE_NAME="${1:-}"
[ -z "$SPRITE_NAME" ] && die "Usage: $0 <sprite-name> --telegram-token <t> --claude-token <t> --webhook-url <url> [options]"
shift

TELEGRAM_TOKEN=""
CLAUDE_TOKEN=""
WEBHOOK_URL=""
AGENT_NAME="Assistant"
AGENT_ID="assistant"
MODEL="opus"
PAIR_TELEGRAM=""

while [ $# -gt 0 ]; do
    case "$1" in
        --telegram-token) TELEGRAM_TOKEN="$2"; shift 2 ;;
        --claude-token)   CLAUDE_TOKEN="$2"; shift 2 ;;
        --webhook-url)    WEBHOOK_URL="$2"; shift 2 ;;
        --agent-name)     AGENT_NAME="$2"; shift 2 ;;
        --agent-id)       AGENT_ID="$2"; shift 2 ;;
        --model)          MODEL="$2"; shift 2 ;;
        --pair-telegram)  PAIR_TELEGRAM="$2"; shift 2 ;;
        *) die "Unknown option: $1" ;;
    esac
done

[ -z "$TELEGRAM_TOKEN" ] && die "--telegram-token is required"
[ -z "$CLAUDE_TOKEN" ]   && die "--claude-token is required"
[ -z "$WEBHOOK_URL" ]    && die "--webhook-url is required"
: "${SPRITES_TOKEN:?SPRITES_TOKEN env var is required}"
: "${B2_KEY_ID:?B2_KEY_ID env var is required}"
: "${B2_APP_KEY:?B2_APP_KEY env var is required}"

# ---------------------------------------------------------------------------
# Step 1: Build Rust binary
# ---------------------------------------------------------------------------

info "Building Rust binary (release)..."
cd "$PROJECT_ROOT"
cargo build --release || die "cargo build failed"
BINARY="$PROJECT_ROOT/target/release/tinyclaw"
[ -f "$BINARY" ] || die "Binary not found at $BINARY"
ok "Binary built ($(du -h "$BINARY" | cut -f1))"

# ---------------------------------------------------------------------------
# Step 2: Create tarball
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

# ---------------------------------------------------------------------------
# Step 3: Upload tarball to B2
# ---------------------------------------------------------------------------

info "Uploading tarball to Backblaze B2..."

# Authorize with B2
B2_AUTH=$(curl -sS "$B2_API/b2api/v3/b2_authorize_account" \
    -u "${B2_KEY_ID}:${B2_APP_KEY}")
B2_API_URL=$(echo "$B2_AUTH" | python3 -c "import sys,json; print(json.load(sys.stdin)['apiInfo']['storageApi']['apiUrl'])")
B2_AUTH_TOKEN=$(echo "$B2_AUTH" | python3 -c "import sys,json; print(json.load(sys.stdin)['authorizationToken'])")
B2_DOWNLOAD_URL=$(echo "$B2_AUTH" | python3 -c "import sys,json; print(json.load(sys.stdin)['apiInfo']['storageApi']['downloadUrl'])")

# Find or detect the B2 bucket (use first available bucket)
B2_BUCKETS=$(curl -sS -X POST \
    -H "Authorization: $B2_AUTH_TOKEN" \
    -d "{\"accountId\":\"$(echo "$B2_AUTH" | python3 -c "import sys,json; print(json.load(sys.stdin)['accountId'])")\"}" \
    "$B2_API_URL/b2api/v3/b2_list_buckets")
B2_BUCKET_ID=$(echo "$B2_BUCKETS" | python3 -c "import sys,json; print(json.load(sys.stdin)['buckets'][0]['bucketId'])")
B2_BUCKET_NAME=$(echo "$B2_BUCKETS" | python3 -c "import sys,json; print(json.load(sys.stdin)['buckets'][0]['bucketName'])")

# Get upload URL
UPLOAD_INFO=$(curl -sS -X POST \
    -H "Authorization: $B2_AUTH_TOKEN" \
    -d "{\"bucketId\":\"$B2_BUCKET_ID\"}" \
    "$B2_API_URL/b2api/v3/b2_get_upload_url")
UPLOAD_URL=$(echo "$UPLOAD_INFO" | python3 -c "import sys,json; print(json.load(sys.stdin)['uploadUrl'])")
UPLOAD_TOKEN=$(echo "$UPLOAD_INFO" | python3 -c "import sys,json; print(json.load(sys.stdin)['authorizationToken'])")

# Upload
SHA1=$(sha1sum "$TARBALL" | cut -d' ' -f1)
UPLOAD_RESULT=$(curl -sS -X POST \
    -H "Authorization: $UPLOAD_TOKEN" \
    -H "X-Bz-File-Name: tinyclaw-deploy.tar.gz" \
    -H "Content-Type: application/gzip" \
    -H "X-Bz-Content-Sha1: $SHA1" \
    --data-binary @"$TARBALL" \
    "$UPLOAD_URL")
B2_FILE_ID=$(echo "$UPLOAD_RESULT" | python3 -c "import sys,json; print(json.load(sys.stdin)['fileId'])")

# Get temporary download authorization (1 hour)
DL_AUTH=$(curl -sS -X POST \
    -H "Authorization: $B2_AUTH_TOKEN" \
    -d "{\"bucketId\":\"$B2_BUCKET_ID\",\"fileNamePrefix\":\"tinyclaw-deploy.tar.gz\",\"validDurationInSeconds\":3600}" \
    "$B2_API_URL/b2api/v3/b2_get_download_authorization")
DL_TOKEN=$(echo "$DL_AUTH" | python3 -c "import sys,json; print(json.load(sys.stdin)['authorizationToken'])")
DOWNLOAD_LINK="${B2_DOWNLOAD_URL}/file/${B2_BUCKET_NAME}/tinyclaw-deploy.tar.gz?Authorization=${DL_TOKEN}"

rm -f "$TARBALL"
ok "Tarball uploaded to B2 (bucket: $B2_BUCKET_NAME)"

# ---------------------------------------------------------------------------
# Step 4: Check sprite + download tarball on sprite
# ---------------------------------------------------------------------------

info "Checking sprite '$SPRITE_NAME'..."
sprite_run "echo ok" > /dev/null || die "Cannot reach sprite '$SPRITE_NAME'"
ok "Sprite is reachable"

info "Downloading tarball on sprite from B2..."
sprite_run "
set -e
rm -rf ${REMOTE_TINYCLAW}
mkdir -p ${REMOTE_HOME}
curl -sS -o /tmp/tinyclaw-deploy.tar.gz '${DOWNLOAD_LINK}'
cd ${REMOTE_HOME} && tar xzf /tmp/tinyclaw-deploy.tar.gz
rm /tmp/tinyclaw-deploy.tar.gz
chmod +x ${REMOTE_TINYCLAW}/tinyclaw
echo DONE
" > /dev/null
ok "Code deployed to sprite"

# Clean up B2 file
curl -sS -X POST \
    -H "Authorization: $B2_AUTH_TOKEN" \
    -d "{\"fileName\":\"tinyclaw-deploy.tar.gz\",\"fileId\":\"$B2_FILE_ID\"}" \
    "$B2_API_URL/b2api/v3/b2_delete_file_version" > /dev/null 2>&1 || true

# ---------------------------------------------------------------------------
# Step 5: Remote setup (dirs, wrapper, configs)
# ---------------------------------------------------------------------------

info "Running remote setup..."

# Dirs + wrapper script
sprite_run "
set -e
mkdir -p ${REMOTE_CONFIG}/{logs,files,chats}
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
sprite-env services create tinyclaw --cmd bash --args '${REMOTE_TINYCLAW}/run.sh' --no-stream
echo DONE
" > /dev/null
ok "Service created and started"

# ---------------------------------------------------------------------------
# Step 7: Verify
# ---------------------------------------------------------------------------

info "Verifying (waiting 5s for startup)..."
sleep 5

LOGS=$(sprite_run "cat /.sprite/logs/services/tinyclaw.log 2>/dev/null | tail -20" || true)
if echo "$LOGS" | grep -q "Webhook listener started"; then
    ok "TinyClaw is running with webhook on port 8080!"
elif echo "$LOGS" | grep -q "TinyClaw starting"; then
    ok "TinyClaw is running!"
else
    warn "Service may still be starting — check logs"
fi

echo ""
echo -e "${GREEN}=== Deployment complete ===${NC}"
echo ""
echo "  Sprite:      $SPRITE_NAME"
echo "  Agent:       $AGENT_NAME ($AGENT_ID) [anthropic/$MODEL]"
echo "  Channel:     Telegram (webhook)"
echo "  Webhook:     $WEBHOOK_URL"
echo "  Service:     tinyclaw (single binary, port 8080)"
echo ""
