#!/usr/bin/env bash
#
# self-update.sh — Download latest release, test, swap binary, restart.
# Safe rollback if the new binary fails to start.
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

# --- Resolve GitHub token ---
if [ -n "${GITHUB_TOKEN:-}" ]; then
    TOKEN="$GITHUB_TOKEN"
elif [ -f "$HOME/.tinyclaw/github-token" ]; then
    TOKEN="$(cat "$HOME/.tinyclaw/github-token")"
else
    echo "ERROR: No GitHub token found. Set GITHUB_TOKEN or create ~/.tinyclaw/github-token"
    exit 1
fi

# --- Parse args ---
TAG=""
while [[ $# -gt 0 ]]; do
    case $1 in
        --tag) TAG="$2"; shift 2 ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

# --- Get release URL ---
if [ -n "$TAG" ]; then
    RELEASE_URL="https://api.github.com/repos/$REPO/releases/tags/$TAG"
else
    RELEASE_URL="https://api.github.com/repos/$REPO/releases/latest"
fi

echo "Fetching release info from $RELEASE_URL ..."
RELEASE_JSON=$(curl -sf -H "Authorization: token $TOKEN" "$RELEASE_URL") || {
    echo "ERROR: Failed to fetch release info. Check token and tag."
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
    echo "ERROR: No $BUNDLE_NAME asset found in release."
    exit 1
fi

RELEASE_TAG=$(echo "$RELEASE_JSON" | python3 -c "import sys,json; print(json.load(sys.stdin)['tag_name'])")
echo "Found release: $RELEASE_TAG"

# --- Download bundle ---
TMPDIR=$(mktemp -d)
trap "rm -rf $TMPDIR" EXIT

echo "Downloading $BUNDLE_NAME ..."
curl -sfL -H "Authorization: token $TOKEN" -H "Accept: application/octet-stream" \
    "$ASSET_URL" -o "$TMPDIR/$BUNDLE_NAME" || {
    echo "ERROR: Failed to download bundle."
    exit 1
}

# --- Extract ---
echo "Extracting ..."
tar -xzf "$TMPDIR/$BUNDLE_NAME" -C "$TMPDIR"
NEW_BINARY="$TMPDIR/tinyclaw/tinyclaw"

if [ ! -f "$NEW_BINARY" ]; then
    echo "ERROR: Binary not found in extracted bundle."
    exit 1
fi

chmod +x "$NEW_BINARY"

# --- Test new binary ---
echo "Testing new binary ..."
if "$NEW_BINARY" --version 2>/dev/null || "$NEW_BINARY" --help >/dev/null 2>&1; then
    echo "New binary looks OK."
else
    # Some binaries don't have --help/--version, just check it's executable
    if file "$NEW_BINARY" | grep -q "ELF"; then
        echo "Binary is valid ELF executable."
    else
        echo "ERROR: New binary doesn't appear to be valid."
        exit 1
    fi
fi

# --- Backup current binary ---
echo "Backing up current binary to $BACKUP_PATH ..."
cp "$BINARY_PATH" "$BACKUP_PATH"

# --- Find and kill current process ---
echo "Stopping current tinyclaw process ..."
PIDS=$(pgrep -f "^./tinyclaw$" 2>/dev/null || true)
if [ -n "$PIDS" ]; then
    kill $PIDS 2>/dev/null || true
    sleep 2
    # Force kill if still running
    kill -9 $PIDS 2>/dev/null || true
fi

# --- Swap binary ---
echo "Swapping binary ..."
cp "$NEW_BINARY" "$BINARY_PATH"

# --- Copy updated supporting files if present ---
for dir in .agents templates scripts; do
    if [ -d "$TMPDIR/tinyclaw/$dir" ]; then
        cp -r "$TMPDIR/tinyclaw/$dir" "/home/user/tinyclaw/"
    fi
done

# --- Restart ---
echo "Starting new tinyclaw ..."
cd /home/user/tinyclaw

# Preserve existing env vars from the running process
WEBHOOK_URL="${WEBHOOK_URL:-}"
WEBHOOK_PORT="${WEBHOOK_PORT:-3000}"

nohup sh -c "WEBHOOK_URL=$WEBHOOK_URL WEBHOOK_PORT=$WEBHOOK_PORT exec ./tinyclaw" > /tmp/tinyclaw-restart.log 2>&1 &
NEW_PID=$!

# --- Health check ---
echo "Waiting ${HEALTH_TIMEOUT}s for process to stabilize ..."
sleep "$HEALTH_TIMEOUT"

if kill -0 "$NEW_PID" 2>/dev/null; then
    echo "SUCCESS: tinyclaw $RELEASE_TAG is running (PID $NEW_PID)"
else
    echo "FAILURE: New binary crashed. Rolling back ..."
    cp "$BACKUP_PATH" "$BINARY_PATH"
    nohup sh -c "WEBHOOK_URL=$WEBHOOK_URL WEBHOOK_PORT=$WEBHOOK_PORT exec ./tinyclaw" > /tmp/tinyclaw-rollback.log 2>&1 &
    ROLLBACK_PID=$!
    sleep 5
    if kill -0 "$ROLLBACK_PID" 2>/dev/null; then
        echo "Rollback successful. Old binary running (PID $ROLLBACK_PID)"
    else
        echo "CRITICAL: Rollback also failed! Manual intervention needed."
    fi
    exit 1
fi
