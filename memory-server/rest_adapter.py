"""
REST adapter for SimpleMem MCP Server.

Adds backward-compatible REST endpoints to the SimpleMem FastAPI app so that
existing skills (which use curl) keep working without changes.

Endpoints:
    POST /api/remember  — store a memory (→ memory_add)
    POST /api/recall    — search memories (→ memory_query)
    POST /api/forget    — clear all memories (→ memory_clear)
    POST /api/memories  — list memories (→ memory_retrieve)
    GET  /api/health    — already provided by SimpleMem

Auth: Bearer token via Authorization header (MEMORY_TOKEN env var).
"""

import json
import os
from typing import Optional
from datetime import datetime

from fastapi import APIRouter, HTTPException, Header
from pydantic import BaseModel

MEMORY_TOKEN = os.environ.get("MEMORY_TOKEN", "")

router = APIRouter(prefix="/api")


# ── Auth ──────────────────────────────────────────────────────────────────

def check_token(authorization: Optional[str]) -> None:
    if not MEMORY_TOKEN:
        return
    if not authorization or authorization != f"Bearer {MEMORY_TOKEN}":
        raise HTTPException(status_code=401, detail="unauthorized")


# ── Request models ────────────────────────────────────────────────────────

class RememberRequest(BaseModel):
    content: str
    speaker: Optional[str] = None
    category: Optional[str] = None
    persons: Optional[list[str]] = None
    location: Optional[str] = None
    entities: Optional[list[str]] = None
    timestamp: Optional[str] = None
    topic: Optional[str] = None


class RecallRequest(BaseModel):
    query: str
    limit: Optional[int] = 10


class ForgetRequest(BaseModel):
    memory_id: Optional[int] = None


class ListRequest(BaseModel):
    category: Optional[str] = None
    limit: Optional[int] = 50
    query: Optional[str] = None


# ── Global reference to MCP handler (set during startup) ─────────────────

_mcp_handler = None
_user = None
_api_key = None


def set_handler(handler, user, api_key):
    """Called at startup to inject the MCP handler for the default user."""
    global _mcp_handler, _user, _api_key
    _mcp_handler = handler
    _user = user
    _api_key = api_key


def _get_handler():
    if _mcp_handler is None:
        raise HTTPException(status_code=503, detail="memory service not initialized")
    return _mcp_handler


# ── Endpoints ─────────────────────────────────────────────────────────────

@router.post("/remember")
async def remember(req: RememberRequest, authorization: Optional[str] = Header(None)):
    check_token(authorization)
    handler = _get_handler()

    speaker = req.speaker or "user"
    if req.persons and not req.speaker:
        speaker = req.persons[0]

    result = await handler._tool_memory_add({
        "speaker": speaker,
        "content": req.content,
        "timestamp": req.timestamp or datetime.utcnow().isoformat(),
    })

    return {
        "stored": True,
        "content": req.content,
        **result,
    }


@router.post("/recall")
async def recall(req: RecallRequest, authorization: Optional[str] = Header(None)):
    check_token(authorization)
    handler = _get_handler()

    result = await handler._tool_memory_query({
        "question": req.query,
        "enable_reflection": False,
    })

    return {
        "query": req.query,
        "answer": result.get("answer", ""),
        "confidence": result.get("confidence", ""),
        "reasoning": result.get("reasoning", ""),
        "contexts_used": result.get("contexts_used", 0),
    }


@router.post("/forget")
async def forget(req: ForgetRequest, authorization: Optional[str] = Header(None)):
    check_token(authorization)
    handler = _get_handler()

    result = await handler._tool_memory_clear({})
    return {"cleared": result.get("success", False)}


@router.post("/memories")
async def memories(req: ListRequest, authorization: Optional[str] = Header(None)):
    check_token(authorization)
    handler = _get_handler()

    query = req.query or "all memories"
    result = await handler._tool_memory_retrieve({
        "query": query,
        "top_k": req.limit or 50,
    })

    return {
        "memories": result.get("results", []),
        "count": result.get("total", 0),
    }
