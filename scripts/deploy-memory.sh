#!/usr/bin/env bash
# =============================================================================
# deploy-memory.sh — Deploy SimpleMem MCP Server to GCP VM (sultana-vm)
# =============================================================================
#
# Uploads the REST adapter, configures SimpleMem, and restarts the service.
# Assumes SimpleMem is already cloned at /home/tinyclaw/SimpleMem with deps
# installed. For first-time setup, see docs or run with --init.
#
# Usage:
#   ./scripts/deploy-memory.sh          # update adapter + restart
#   ./scripts/deploy-memory.sh --init   # full setup (clone, install, configure)
# =============================================================================

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
NC='\033[0m'

info() { echo -e "${BLUE}→${NC} $1"; }
ok()   { echo -e "${GREEN}✓${NC} $1"; }
die()  { echo -e "${RED}Error: $1${NC}" >&2; exit 1; }

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SSH_KEY="$HOME/.ssh/google_compute_engine"
SSH_USER="shah"
SSH_HOST="104.154.140.203"
SSH_CMD="ssh -i $SSH_KEY -o StrictHostKeyChecking=no $SSH_USER@$SSH_HOST"
SCP_CMD="scp -i $SSH_KEY -o StrictHostKeyChecking=no"

REMOTE_SIMPLEMEM="/home/tinyclaw/SimpleMem/MCP"
REMOTE_DATA="/home/tinyclaw/.tinyclaw/simplemem-data"

# Load .env
if [[ -f "$PROJECT_ROOT/.env" ]]; then
    set -a
    source "$PROJECT_ROOT/.env"
    set +a
fi

[[ -n "${MEMORY_TOKEN:-}" ]] || die "MEMORY_TOKEN not set in .env"
[[ -n "${OPENROUTER_API_KEY:-}" ]] || die "OPENROUTER_API_KEY not set in .env"
[[ -f "$SSH_KEY" ]] || die "SSH key not found: $SSH_KEY"

INIT=false
[[ "${1:-}" == "--init" ]] && INIT=true

# ── Full setup (first time only) ─────────────────────────────────────
if $INIT; then
    info "Installing uv..."
    $SSH_CMD "sudo -u tinyclaw bash -c 'curl -LsSf https://astral.sh/uv/install.sh | sh'" || true

    info "Cloning SimpleMem..."
    $SSH_CMD "sudo -u tinyclaw bash -c 'cd /home/tinyclaw && rm -rf SimpleMem && git clone --depth 1 https://github.com/aiming-lab/SimpleMem.git'"

    info "Installing dependencies..."
    $SSH_CMD "sudo -u tinyclaw bash -c 'cd $REMOTE_SIMPLEMEM && /home/tinyclaw/.local/bin/uv venv .venv && /home/tinyclaw/.local/bin/uv pip install -r requirements.txt'"
    ok "SimpleMem installed"
fi

# ── Upload REST adapter ──────────────────────────────────────────────
info "Uploading REST adapter..."
$SCP_CMD "$PROJECT_ROOT/memory-server/rest_adapter.py" "$SSH_USER@$SSH_HOST:/tmp/rest_adapter.py"
$SCP_CMD "$PROJECT_ROOT/memory-server/start_server.py" "$SSH_USER@$SSH_HOST:/tmp/start_server.py"
$SSH_CMD "sudo mv /tmp/rest_adapter.py /tmp/start_server.py $REMOTE_SIMPLEMEM/ && sudo chown tinyclaw:tinyclaw $REMOTE_SIMPLEMEM/rest_adapter.py $REMOTE_SIMPLEMEM/start_server.py"
ok "REST adapter uploaded"

# ── Write .env ───────────────────────────────────────────────────────
info "Writing config..."
JWT_SECRET=$(openssl rand -hex 32)
ENCRYPTION_KEY=$(openssl rand -hex 16)

$SSH_CMD "sudo -u tinyclaw bash -c 'mkdir -p $REMOTE_DATA/lancedb && cat > $REMOTE_SIMPLEMEM/.env << ENVEOF
LLM_PROVIDER=openrouter
OPENROUTER_BASE_URL=https://openrouter.ai/api/v1
LLM_MODEL=google/gemini-3-flash-preview
EMBEDDING_MODEL=qwen/qwen3-embedding-8b
EMBEDDING_DIMENSION=4096
DATA_DIR=$REMOTE_DATA
LANCEDB_PATH=$REMOTE_DATA/lancedb
USER_DB_PATH=$REMOTE_DATA/users.db
JWT_SECRET_KEY=$JWT_SECRET
ENCRYPTION_KEY=$ENCRYPTION_KEY
MCP_BASE_URL=http://$SSH_HOST:8642
MEMORY_TOKEN=$MEMORY_TOKEN
OPENROUTER_API_KEY=$OPENROUTER_API_KEY
ENVEOF
chmod 600 $REMOTE_SIMPLEMEM/.env'"
ok "Config written"

# ── Create systemd service ───────────────────────────────────────────
info "Creating systemd service..."
$SSH_CMD "sudo tee /etc/systemd/system/memory-server.service > /dev/null" <<'UNIT'
[Unit]
Description=SimpleMem MCP Server + REST Adapter
After=network.target

[Service]
Type=simple
User=tinyclaw
Group=tinyclaw
WorkingDirectory=/home/tinyclaw/SimpleMem/MCP
EnvironmentFile=/home/tinyclaw/SimpleMem/MCP/.env
Environment=SIMPLEMEM_MCP_PATH=/home/tinyclaw/SimpleMem/MCP
ExecStart=/home/tinyclaw/SimpleMem/MCP/.venv/bin/uvicorn start_server:app --host 0.0.0.0 --port 8642
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
UNIT
ok "Systemd service created"

# ── Start service ────────────────────────────────────────────────────
info "Starting SimpleMem..."
$SSH_CMD "sudo systemctl daemon-reload && sudo systemctl enable memory-server && sudo systemctl restart memory-server"
sleep 4

# Verify
STATUS=$($SSH_CMD "sudo systemctl is-active memory-server" 2>/dev/null || true)
if [[ "$STATUS" == "active" ]]; then
    ok "SimpleMem running on port 8642"
else
    die "Service failed to start. Check: ssh $SSH_USER@$SSH_HOST 'sudo journalctl -u memory-server -n 20'"
fi

# ── Open firewall ────────────────────────────────────────────────────
info "Checking GCP firewall..."
if command -v gcloud &>/dev/null; then
    RULE_EXISTS=$(gcloud compute firewall-rules list --filter="name=allow-memory-server" --format="value(name)" 2>/dev/null || true)
    if [[ -z "$RULE_EXISTS" ]]; then
        info "Creating firewall rule..."
        gcloud compute firewall-rules create allow-memory-server \
            --allow=tcp:8642 \
            --target-tags=memory-server \
            --description="Allow memory server access" \
            --quiet 2>/dev/null || echo "  (manual firewall setup may be needed)"
        ok "Firewall rule created"
    else
        ok "Firewall rule exists"
    fi
else
    echo "  gcloud not found — ensure port 8642 is open in GCP console"
fi

# ── Test ─────────────────────────────────────────────────────────────
info "Testing API..."
HEALTH=$(curl -sf -m 5 "http://$SSH_HOST:8642/api/health" 2>/dev/null || echo "FAILED")
SERVER_INFO=$(curl -sf -m 5 "http://$SSH_HOST:8642/api/server/info" 2>/dev/null || echo "{}")

if [[ "$HEALTH" == "FAILED" ]]; then
    echo "  API not reachable (firewall may need manual config)"
    echo "  Test locally: ssh $SSH_USER@$SSH_HOST 'curl -s http://localhost:8642/api/health'"
else
    ok "API responding: $HEALTH"
    echo "  Server: $SERVER_INFO"
fi

# ── Summary ──────────────────────────────────────────────────────────
echo
echo -e "${GREEN}=== SimpleMem Deployed ===${NC}"
echo "  URL:       http://$SSH_HOST:8642"
echo "  Backend:   SimpleMem MCP (LanceDB + OpenRouter)"
echo "  LLM:       google/gemini-3-flash-preview"
echo "  Embedding: qwen/qwen3-embedding-8b"
echo "  Data:      $REMOTE_DATA"
echo "  Service:   memory-server (systemd)"
echo "  Logs:      ssh $SSH_USER@$SSH_HOST 'sudo journalctl -u memory-server -f'"
echo
