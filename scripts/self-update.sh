#!/usr/bin/env bash
#
# self-update.sh — Download latest release, test, swap binary, restart.
# Safe rollback if the new binary fails to start.
# Writes a detailed update report to ~/.tinyclaw/last-update.log
#
# Usage: ./self-update.sh [--tag vX.Y.Z]
#
# Requires: GITHUB_TOKEN env var or ~/.tinyclaw/github-token file

set -euo pipefail

REPO="cloudrumbles/tinyclaw"
BINARY_PATH="/home/user/tinyclaw/tinyclaw"
BACKUP_PATH="/home/user/tinyclaw/tinyclaw.bak"
BUNDLE_NAME="tinyclaw-bundle.tar.gz"
HEALTH_TIMEOUT=15  # seconds to wait for process to be healthy
UPDATE_LOG="$HOME/.tinyclaw/update-history.log"
PROCESS_LOG="/tmp/tinyclaw-restart.log"

# --- Logging ---
log() {
    local ts
    ts=$(date '+%Y-%m-%d %H:%M:%S SGT')
    echo "[$ts] $*"
    echo "[$ts] $*" >> "$UPDATE_LOG"
}

mkdir -p "$(dirname "$UPDATE_LOG")"
echo "" >> "$UPDATE_LOG"
log "=== SELF-UPDATE STARTED ==="

# --- Resolve GitHub token ---
if [ -n "${GITHUB_TOKEN:-}" ]; then
    TOKEN="$GITHUB_TOKEN"
elif [ -f "$HOME/.tinyclaw/github-token" ]; then
    TOKEN="$(cat "$HOME/.tinyclaw/github-token")"
else
    log "ERROR: No GitHub token found. Set GITHUB_TOKEN or create ~/.tinyclaw/github-token"
    exit 1
fi

# --- Parse args ---
TAG=""
while [[ $# -gt 0 ]]; do
    case $1 in
        --tag) TAG="$2"; shift 2 ;;
        *) log "Unknown option: $1"; exit 1 ;;
    esac
done

# --- Get release URL ---
if [ -n "$TAG" ]; then
    RELEASE_URL="https://api.github.com/repos/$REPO/releases/tags/$TAG"
else
    RELEASE_URL="https://api.github.com/repos/$REPO/releases/latest"
fi

log "Fetching release info..."
RELEASE_JSON=$(curl -sf -H "Authorization: token $TOKEN" "$RELEASE_URL") || {
    log "ERROR: Failed to fetch release info. Check token and tag."
    exit 1
}

ASSET_URL=$(echo "$RELEASE_JSON" | python3 -c "
import sys, json
data = json.load(sys.stdin)
for asset in data.get('assets', []):
    if asset['name'] == '$BUNDLE_NAME':
        print(asset['url'])
        break
" 2>/dev/null) || true

if [ -z "$ASSET_URL" ]; then
    log "ERROR: No $BUNDLE_NAME asset found in release."
    exit 1
fi

RELEASE_TAG=$(echo "$RELEASE_JSON" | python3 -c "import sys,json; print(json.load(sys.stdin)['tag_name'])")
log "Found release: $RELEASE_TAG"

# --- Save current version info ---
OLD_BINARY_HASH=$(md5sum "$BINARY_PATH" 2>/dev/null | cut -d' ' -f1 || echo "unknown")
log "Current binary hash: $OLD_BINARY_HASH"

# --- Download bundle ---
TMPDIR=$(mktemp -d)
trap "rm -rf $TMPDIR" EXIT

log "Downloading $BUNDLE_NAME..."
curl -sfL -H "Authorization: token $TOKEN" -H "Accept: application/octet-stream" \
    "$ASSET_URL" -o "$TMPDIR/$BUNDLE_NAME" || {
    log "ERROR: Failed to download bundle."
    exit 1
}

# --- Extract ---
log "Extracting..."
tar -xzf "$TMPDIR/$BUNDLE_NAME" -C "$TMPDIR"
NEW_BINARY="$TMPDIR/tinyclaw/tinyclaw"

if [ ! -f "$NEW_BINARY" ]; then
    log "ERROR: Binary not found in extracted bundle."
    exit 1
fi

chmod +x "$NEW_BINARY"
NEW_BINARY_HASH=$(md5sum "$NEW_BINARY" | cut -d' ' -f1)
log "New binary hash: $NEW_BINARY_HASH"

if [ "$OLD_BINARY_HASH" = "$NEW_BINARY_HASH" ]; then
    log "WARNING: New binary is identical to current. Skipping update."
    log "=== SELF-UPDATE SKIPPED (no change) ==="
    exit 0
fi

# --- Test new binary ---
log "Testing new binary (ELF check)..."
if file "$NEW_BINARY" | grep -q "ELF"; then
    log "Binary is valid ELF executable."
else
    log "ERROR: New binary doesn't appear to be a valid ELF executable."
    log "file output: $(file "$NEW_BINARY")"
    exit 1
fi

# --- Backup current binary ---
log "Backing up current binary to $BACKUP_PATH"
cp "$BINARY_PATH" "$BACKUP_PATH"

# --- Capture current env from running process ---
CURRENT_PID=$(pgrep -f "^./tinyclaw$" 2>/dev/null | head -1 || true)
WEBHOOK_URL="${WEBHOOK_URL:-}"
WEBHOOK_PORT="${WEBHOOK_PORT:-3000}"

if [ -n "$CURRENT_PID" ]; then
    # Try to read env from running process
    WEBHOOK_URL=$(tr '\0' '\n' < /proc/$CURRENT_PID/environ 2>/dev/null | grep "^WEBHOOK_URL=" | cut -d= -f2- || echo "$WEBHOOK_URL")
    WEBHOOK_PORT=$(tr '\0' '\n' < /proc/$CURRENT_PID/environ 2>/dev/null | grep "^WEBHOOK_PORT=" | cut -d= -f2- || echo "$WEBHOOK_PORT")
    log "Captured env from PID $CURRENT_PID: WEBHOOK_URL=$WEBHOOK_URL WEBHOOK_PORT=$WEBHOOK_PORT"
fi

# --- Stop current process ---
log "Stopping current tinyclaw process..."
PIDS=$(pgrep -f "^./tinyclaw$" 2>/dev/null || true)
if [ -n "$PIDS" ]; then
    kill $PIDS 2>/dev/null || true
    sleep 2
    # Force kill if still running
    kill -9 $PIDS 2>/dev/null || true
    log "Killed PIDs: $PIDS"
else
    log "WARNING: No running tinyclaw process found."
fi

# --- Swap binary ---
log "Swapping binary..."
cp "$NEW_BINARY" "$BINARY_PATH"

# --- Copy updated supporting files if present ---
for dir in .agents templates scripts; do
    if [ -d "$TMPDIR/tinyclaw/$dir" ]; then
        cp -r "$TMPDIR/tinyclaw/$dir" "/home/user/tinyclaw/"
        log "Updated $dir/"
    fi
done

# --- Restart ---
log "Starting new tinyclaw..."
cd /home/user/tinyclaw

# Clear the process log so we only see output from this run
> "$PROCESS_LOG"

nohup sh -c "WEBHOOK_URL=$WEBHOOK_URL WEBHOOK_PORT=$WEBHOOK_PORT exec ./tinyclaw" > "$PROCESS_LOG" 2>&1 &
NEW_PID=$!
log "Started new process (PID $NEW_PID)"

# --- Health check ---
log "Waiting ${HEALTH_TIMEOUT}s for process to stabilize..."
sleep "$HEALTH_TIMEOUT"

if kill -0 "$NEW_PID" 2>/dev/null; then
    log "=== SELF-UPDATE SUCCESS: $RELEASE_TAG running (PID $NEW_PID) ==="

    # Capture first few lines of startup output for the record
    log "--- Startup output (first 20 lines) ---"
    head -20 "$PROCESS_LOG" >> "$UPDATE_LOG" 2>/dev/null || true
    log "--- End startup output ---"
else
    log "FAILURE: New binary crashed after ${HEALTH_TIMEOUT}s!"

    # Capture the crash output - this is the key observability bit
    log "--- CRASH OUTPUT START ---"
    cat "$PROCESS_LOG" >> "$UPDATE_LOG" 2>/dev/null || true
    log "--- CRASH OUTPUT END ---"

    # Also capture exit code if possible
    wait "$NEW_PID" 2>/dev/null
    EXIT_CODE=$?
    log "Process exit code: $EXIT_CODE"

    # Check for core dumps
    CORE_FILE=$(ls -t /tmp/core.* 2>/dev/null | head -1 || true)
    if [ -n "$CORE_FILE" ]; then
        log "Core dump found: $CORE_FILE"
    fi

    # Rollback
    log "Rolling back to previous binary..."
    cp "$BACKUP_PATH" "$BINARY_PATH"

    > "$PROCESS_LOG"
    nohup sh -c "WEBHOOK_URL=$WEBHOOK_URL WEBHOOK_PORT=$WEBHOOK_PORT exec ./tinyclaw" > "$PROCESS_LOG" 2>&1 &
    ROLLBACK_PID=$!
    sleep 5

    if kill -0 "$ROLLBACK_PID" 2>/dev/null; then
        log "=== ROLLBACK SUCCESS: Old binary running (PID $ROLLBACK_PID) ==="
    else
        log "=== CRITICAL: ROLLBACK ALSO FAILED ==="
        log "--- Rollback crash output ---"
        cat "$PROCESS_LOG" >> "$UPDATE_LOG" 2>/dev/null || true
        log "Manual intervention required!"
    fi
    exit 1
fi
