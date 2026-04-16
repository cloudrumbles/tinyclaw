#!/usr/bin/env python3
"""
Memory HTTP API server — lightweight, zero-dependency.

Exposes a REST API for storing/retrieving memories backed by SQLite + FTS5.
Inspired by SimpleMem's atomic memory entries and keyword-based retrieval.

Endpoints:
    POST /api/remember  — store a memory
    POST /api/recall    — search memories
    POST /api/forget    — delete a memory
    POST /api/memories  — list memories
    GET  /api/health    — health check

Auth: Bearer token via Authorization header (set MEMORY_TOKEN env var).
"""

import json
import os
import sys
from http.server import HTTPServer, BaseHTTPRequestHandler
from pathlib import Path

# Import memory_db from same directory
sys.path.insert(0, str(Path(__file__).parent))
from memory_db import MemoryDB

MEMORY_DB_PATH = os.environ.get("MEMORY_DB_PATH", str(Path.home() / ".tinyclaw" / "memory.db"))
MEMORY_TOKEN = os.environ.get("MEMORY_TOKEN", "")
MEMORY_PORT = int(os.environ.get("MEMORY_PORT", "8642"))

db: MemoryDB | None = None


def check_auth(headers: dict) -> bool:
    """Verify bearer token. If MEMORY_TOKEN is empty, auth is disabled."""
    if not MEMORY_TOKEN:
        return True
    auth = headers.get("Authorization", "")
    return auth == f"Bearer {MEMORY_TOKEN}"


def json_response(handler: BaseHTTPRequestHandler, status: int, data: dict):
    body = json.dumps(data).encode()
    handler.send_response(status)
    handler.send_header("Content-Type", "application/json")
    handler.send_header("Content-Length", str(len(body)))
    handler.end_headers()
    handler.wfile.write(body)


def read_body(handler: BaseHTTPRequestHandler) -> dict:
    length = int(handler.headers.get("Content-Length", 0))
    if length == 0:
        return {}
    raw = handler.rfile.read(length)
    return json.loads(raw)


def handle_remember(body: dict) -> tuple[int, dict]:
    content = body.get("content", "").strip()
    if not content:
        return 400, {"error": "content is required"}

    memory_id = db.store(
        content=content,
        category=body.get("category"),
        persons=body.get("persons"),
        location=body.get("location"),
        entities=body.get("entities"),
        timestamp=body.get("timestamp"),
        topic=body.get("topic"),
    )
    return 200, {"id": memory_id, "content": content, "stored": True}


def handle_recall(body: dict) -> tuple[int, dict]:
    query = body.get("query", "").strip()
    if not query:
        return 400, {"error": "query is required"}

    limit = body.get("limit", 10)
    memories = db.search(query, limit=limit)
    return 200, {"memories": memories, "count": len(memories), "query": query}


def handle_forget(body: dict) -> tuple[int, dict]:
    memory_id = body.get("memory_id")
    if memory_id is None:
        return 400, {"error": "memory_id is required"}

    deleted = db.delete(int(memory_id))
    return 200, {"deleted": deleted, "memory_id": memory_id}


def handle_memories(body: dict) -> tuple[int, dict]:
    category = body.get("category")
    limit = body.get("limit", 50)
    memories = db.list_all(category=category, limit=limit)
    return 200, {"memories": memories, "count": len(memories)}


def handle_health() -> tuple[int, dict]:
    stats = db.stats()
    return 200, {"status": "ok", **stats}


ROUTES = {
    "/api/remember": handle_remember,
    "/api/recall": handle_recall,
    "/api/forget": handle_forget,
    "/api/memories": handle_memories,
}


class MemoryHandler(BaseHTTPRequestHandler):
    def do_GET(self):
        if not check_auth(self.headers):
            return json_response(self, 401, {"error": "unauthorized"})

        if self.path == "/api/health":
            status, data = handle_health()
            return json_response(self, status, data)

        json_response(self, 404, {"error": "not found"})

    def do_POST(self):
        if not check_auth(self.headers):
            return json_response(self, 401, {"error": "unauthorized"})

        handler = ROUTES.get(self.path)
        if not handler:
            return json_response(self, 404, {"error": "not found"})

        try:
            body = read_body(self)
            status, data = handler(body)
            json_response(self, status, data)
        except json.JSONDecodeError:
            json_response(self, 400, {"error": "invalid JSON"})
        except Exception as e:
            json_response(self, 500, {"error": str(e)})

    def log_message(self, format, *args):
        """Override to log to stderr with timestamp."""
        sys.stderr.write(f"[memory] {args[0]} {args[1]} {args[2]}\n")


def main():
    global db
    print(f"[memory] db: {MEMORY_DB_PATH}", file=sys.stderr)
    print(f"[memory] port: {MEMORY_PORT}", file=sys.stderr)
    print(f"[memory] auth: {'enabled' if MEMORY_TOKEN else 'disabled'}", file=sys.stderr)

    db = MemoryDB(MEMORY_DB_PATH)

    server = HTTPServer(("0.0.0.0", MEMORY_PORT), MemoryHandler)
    print(f"[memory] listening on 0.0.0.0:{MEMORY_PORT}", file=sys.stderr)

    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        db.close()
        server.server_close()
        print("[memory] stopped", file=sys.stderr)


if __name__ == "__main__":
    main()
