---
name: memory
description: Persistent long-term memory powered by SimpleMem. Stores facts as vector embeddings with semantic search, LLM-powered compression, and entity extraction. Triggers on phrases like "remember", "recall", "what did I say", "do you remember", "my preference", "store this", "forget".
---

# Long-term Memory (SimpleMem)

You have persistent semantic memory powered by SimpleMem. It stores memories as vector embeddings with automatic entity extraction, coreference resolution, and temporal anchoring. Recall uses hybrid retrieval (semantic similarity + keyword matching) with LLM-generated answers.

## Configuration

Before calling the API, read the config:

```bash
source ~/.tinyclaw/memory.conf
```

This sets `MEMORY_URL` and `MEMORY_TOKEN`.

## API Reference

### Store a memory

```bash
curl -s -X POST "$MEMORY_URL/api/remember" \
  -H "Authorization: Bearer $MEMORY_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"content": "...", "speaker": "Shah", "persons": ["Shah"], "topic": "..."}'
```

**Fields:**
- `content` (required) — the fact or dialogue to store. SimpleMem automatically extracts entities, resolves pronouns, and anchors timestamps.
- `speaker` — who said this (used for coreference resolution). Defaults to "user".
- `persons` — array of person names mentioned
- `location` — location if relevant
- `entities` — array of entities (companies, products, tools)
- `timestamp` — ISO 8601 date if relevant
- `topic` — short topic phrase

### Search memories (semantic + AI answer)

The recall endpoint uses semantic vector search and returns an AI-generated answer synthesized from relevant memories.

```bash
curl -s -X POST "$MEMORY_URL/api/recall" \
  -H "Authorization: Bearer $MEMORY_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"query": "What editor does Shah prefer?"}'
```

**Response includes:**
- `answer` — AI-generated answer from stored memories
- `confidence` — high/medium/low
- `reasoning` — why the AI gave this answer
- `contexts_used` — number of memory entries consulted

### Clear all memories

```bash
curl -s -X POST "$MEMORY_URL/api/forget" \
  -H "Authorization: Bearer $MEMORY_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{}'
```

**WARNING:** This clears ALL memories. There is no single-memory delete.

### Browse memories

```bash
curl -s -X POST "$MEMORY_URL/api/memories" \
  -H "Authorization: Bearer $MEMORY_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"query": "Shah preferences", "limit": 20}'
```

Returns raw memory entries with metadata (content, timestamp, location, persons, entities, topic).

### Health check

```bash
curl -s "$MEMORY_URL/api/health" -H "Authorization: Bearer $MEMORY_TOKEN"
```

## How SimpleMem Works

When you store a memory, the system:
1. Sends the content to an LLM for semantic compression
2. Resolves pronouns to actual names (he → Shah)
3. Converts relative times to absolute (tomorrow → 2026-02-18)
4. Extracts structured metadata (persons, locations, entities, topics)
5. Generates a vector embedding and stores it in LanceDB

When you recall, the system:
1. Embeds the query as a vector
2. Searches by semantic similarity AND keyword matching
3. Synthesizes an AI answer from the most relevant memories

## When to Use

**Store memories when:**
- User states a preference
- An important decision is made
- A significant event happens
- User explicitly asks you to remember something
- You learn a new fact about the user or project

**Recall memories when:**
- User asks about something from a past conversation
- You need context about preferences or past decisions
- Historical context would improve your answer
- At conversation start, to re-orient yourself
