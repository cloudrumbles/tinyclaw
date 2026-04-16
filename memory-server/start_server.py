"""
Startup wrapper for SimpleMem MCP Server + REST adapter.

Initializes the default user's MCP handler (using OPENROUTER_API_KEY from env)
and mounts backward-compatible REST endpoints alongside the MCP server.

Usage:
    uvicorn start_server:app --host 0.0.0.0 --port 8642
"""

import os
import sys

# Add SimpleMem MCP to path
SIMPLEMEM_MCP = os.environ.get(
    "SIMPLEMEM_MCP_PATH",
    os.path.expanduser("~/SimpleMem/MCP"),
)
sys.path.insert(0, SIMPLEMEM_MCP)

from server.http_server import (
    app,
    user_store,
    vector_store,
    token_manager,
    client_manager,
    settings,
)
from server.mcp_handler import MCPHandler
from server.auth.models import User

# Import our REST adapter
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from rest_adapter import router as rest_router, set_handler

MEMORY_TOKEN = os.environ.get("MEMORY_TOKEN", "")
OPENROUTER_API_KEY = os.environ.get("OPENROUTER_API_KEY", "")


def _get_or_create_default_user() -> tuple:
    """Get existing user or register a new one. Returns (user, api_key)."""
    # Check if any user exists
    users = user_store.list_users()
    if users:
        user = users[0]
        api_key = token_manager.decrypt_api_key(user.openrouter_api_key_encrypted)
        return user, api_key

    # Register new user with the env API key
    if not OPENROUTER_API_KEY:
        print("[rest] WARNING: No OPENROUTER_API_KEY set and no existing user", file=sys.stderr)
        return None, None

    user = User()
    user.openrouter_api_key_encrypted = token_manager.encrypt_api_key(OPENROUTER_API_KEY)
    user_store.create_user(user)
    print(f"[rest] Created default user: {user.user_id}", file=sys.stderr)
    return user, OPENROUTER_API_KEY


# Wire up the REST adapter
user, api_key = _get_or_create_default_user()
if user and api_key:
    handler = MCPHandler(
        user=user,
        api_key=api_key,
        vector_store=vector_store,
        client_manager=client_manager,
        settings=settings,
    )
    set_handler(handler, user, api_key)
    print(f"[rest] REST adapter ready (token auth: {'enabled' if MEMORY_TOKEN else 'disabled'})", file=sys.stderr)
else:
    print("[rest] REST adapter disabled (no user)", file=sys.stderr)

# Mount REST routes onto the SimpleMem app
app.include_router(rest_router)
