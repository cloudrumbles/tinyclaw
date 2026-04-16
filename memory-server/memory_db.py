"""
Memory storage layer — SQLite + FTS5.

Inspired by SimpleMem's MemoryEntry schema and lexical retrieval layer.
Each memory is a self-contained atomic fact with structured metadata,
indexed for full-text search via FTS5.

Zero external dependencies — uses only Python stdlib.
"""

import json
import re
import sqlite3
from pathlib import Path

STOPWORDS = frozenset({
    "a", "about", "an", "and", "are", "as", "at", "be", "been", "but", "by",
    "can", "do", "for", "from", "had", "has", "have", "he", "her", "his",
    "how", "i", "if", "in", "into", "is", "it", "its", "just", "me", "my",
    "no", "not", "of", "on", "or", "our", "out", "she", "so", "some",
    "than", "that", "the", "their", "them", "then", "there", "they", "this",
    "to", "up", "us", "was", "we", "were", "what", "when", "where", "which",
    "who", "will", "with", "would", "you", "your",
})

WORD_RE = re.compile(r"\b[a-zA-Z0-9]{2,}\b")

SCHEMA = """
CREATE TABLE IF NOT EXISTS memories (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    content TEXT NOT NULL,
    category TEXT,
    keywords TEXT,
    persons TEXT,
    location TEXT,
    entities TEXT,
    timestamp TEXT,
    topic TEXT,
    created_at TEXT DEFAULT (datetime('now'))
);

CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
    content, keywords, topic,
    content='memories',
    content_rowid='id'
);

CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
    INSERT INTO memories_fts(rowid, content, keywords, topic)
    VALUES (new.id, new.content, COALESCE(new.keywords, ''), COALESCE(new.topic, ''));
END;

CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
    INSERT INTO memories_fts(memories_fts, rowid, content, keywords, topic)
    VALUES ('delete', old.id, old.content, COALESCE(old.keywords, ''), COALESCE(old.topic, ''));
END;

CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE ON memories BEGIN
    INSERT INTO memories_fts(memories_fts, rowid, content, keywords, topic)
    VALUES ('delete', old.id, old.content, COALESCE(old.keywords, ''), COALESCE(old.topic, ''));
    INSERT INTO memories_fts(rowid, content, keywords, topic)
    VALUES (new.id, new.content, COALESCE(new.keywords, ''), COALESCE(new.topic, ''));
END;
"""


class MemoryDB:
    def __init__(self, db_path: str):
        Path(db_path).parent.mkdir(parents=True, exist_ok=True)
        self.conn = sqlite3.connect(db_path)
        self.conn.row_factory = sqlite3.Row
        self.conn.execute("PRAGMA journal_mode=WAL")
        self.conn.execute("PRAGMA foreign_keys=ON")
        self._init_schema()

    def _init_schema(self):
        self.conn.executescript(SCHEMA)
        self.conn.commit()

    @staticmethod
    def extract_keywords(text: str) -> str:
        """Extract searchable keywords from text (lowercased, stopwords removed)."""
        words = WORD_RE.findall(text.lower())
        return " ".join(w for w in dict.fromkeys(words) if w not in STOPWORDS)

    def store(
        self,
        content: str,
        category: str | None = None,
        persons: list[str] | None = None,
        location: str | None = None,
        entities: list[str] | None = None,
        timestamp: str | None = None,
        topic: str | None = None,
    ) -> int:
        """Store a memory. Returns the memory ID."""
        keywords = self.extract_keywords(content)
        # Append metadata terms to keywords for better recall
        if persons:
            keywords += " " + " ".join(p.lower() for p in persons)
        if entities:
            keywords += " " + " ".join(e.lower() for e in entities)
        if location:
            keywords += " " + location.lower()

        cur = self.conn.execute(
            """INSERT INTO memories (content, category, keywords, persons, location, entities, timestamp, topic)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?)""",
            (
                content,
                category,
                keywords.strip(),
                json.dumps(persons) if persons else None,
                location,
                json.dumps(entities) if entities else None,
                timestamp,
                topic,
            ),
        )
        self.conn.commit()
        return cur.lastrowid

    def search(self, query: str, limit: int = 10) -> list[dict]:
        """Full-text search across content, keywords, and topic."""
        # Build FTS5 query: quote each token so special chars don't break syntax
        tokens = WORD_RE.findall(query.lower())
        if not tokens:
            return []

        fts_query = " OR ".join(f'"{t}"' for t in tokens)

        rows = self.conn.execute(
            """SELECT m.*, rank
               FROM memories_fts fts
               JOIN memories m ON m.id = fts.rowid
               WHERE memories_fts MATCH ?
               ORDER BY rank
               LIMIT ?""",
            (fts_query, limit),
        ).fetchall()

        return [self._row_to_dict(r) for r in rows]

    def delete(self, memory_id: int) -> bool:
        """Delete a memory by ID. Returns True if found and deleted."""
        cur = self.conn.execute("DELETE FROM memories WHERE id = ?", (memory_id,))
        self.conn.commit()
        return cur.rowcount > 0

    def list_all(self, category: str | None = None, limit: int = 50) -> list[dict]:
        """List memories, newest first. Optional category filter."""
        if category:
            rows = self.conn.execute(
                "SELECT * FROM memories WHERE category = ? ORDER BY created_at DESC LIMIT ?",
                (category, limit),
            ).fetchall()
        else:
            rows = self.conn.execute(
                "SELECT * FROM memories ORDER BY created_at DESC LIMIT ?",
                (limit,),
            ).fetchall()

        return [self._row_to_dict(r) for r in rows]

    def stats(self) -> dict:
        """Return memory counts."""
        total = self.conn.execute("SELECT COUNT(*) FROM memories").fetchone()[0]
        categories = self.conn.execute(
            "SELECT category, COUNT(*) as cnt FROM memories GROUP BY category ORDER BY cnt DESC"
        ).fetchall()
        return {
            "total": total,
            "by_category": {r["category"] or "uncategorized": r["cnt"] for r in categories},
        }

    def _row_to_dict(self, row: sqlite3.Row) -> dict:
        d = dict(row)
        d.pop("rank", None)
        # Parse JSON arrays back to lists
        for field in ("persons", "entities"):
            if d.get(field):
                try:
                    d[field] = json.loads(d[field])
                except (json.JSONDecodeError, TypeError):
                    pass
        return d

    def close(self):
        self.conn.close()
