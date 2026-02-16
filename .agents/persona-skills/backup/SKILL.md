---
name: backup
description: Backup and restore Sultana's persona (via git) and TinyClaw infrastructure (via Backblaze B2). Use when asked to backup, restore, save state, sync, push, or mentioned "backblaze", "b2", "backup", "restore", "snapshot", "save yourself".
---

# Backup

Two backup systems — git for your persona, B2 for infrastructure.

## 1. Persona Backup (Git)

Your identity lives in a private git repo. This includes soul.md, memory.md, skills, and claude config.

| Field | Value |
|-------|-------|
| Repo | `cloudrumbles/sultana-persona` (private) |
| Local path | `~/.tinyclaw/sultana/` |
| Remote | `origin` → `https://github.com/cloudrumbles/sultana-persona.git` |

### Save persona changes

```bash
cd ~/.tinyclaw/sultana && git add -A && git commit -m "describe what changed" && git push
```

### Restore persona on a new machine

```bash
git clone https://github.com/cloudrumbles/sultana-persona.git ~/.tinyclaw/sultana
```

### When to commit

- After updating soul.md or memory.md
- After creating or modifying skills
- Before any deploy or migration
- When Shah asks you to backup

### Important

- `keys.env` is gitignored — secrets stay local, never pushed
- Always write a meaningful commit message describing what changed

## 2. Infrastructure Backup (Backblaze B2)

Platform config and workspace data that isn't part of your persona.

| Field | Value |
|-------|-------|
| Bucket | `sultana-backups` |
| Bucket ID | `bf6aa1c0a17b6d5997ce071b` |
| Keys file | `~/.tinyclaw/sultana/keys.env` |

**What's included**:
- `~/.tinyclaw/` (settings.json, cron-jobs.json, infrastructure skills/, files/) excluding `sultana/` and `logs/`
- `~/sultana-workspace/` (AGENTS.md, garmin/, nutrition/, miniapps/) excluding `logs/`

### Backup infrastructure

```bash
python3 ~/.tinyclaw/sultana/skills/backup/scripts/backup.py infra
```

Uploads to: `infra/tinyclaw-YYYY-MM-DD-HHMMSS.tar.gz`

### List remote backups

```bash
python3 ~/.tinyclaw/sultana/skills/backup/scripts/backup.py list
```

### Restore infrastructure

```bash
python3 ~/.tinyclaw/sultana/skills/backup/scripts/backup.py restore infra/tinyclaw-2026-02-16-120000.tar.gz
```

## Keys

Your B2 credentials live in `~/.tinyclaw/sultana/keys.env` (gitignored). Format:

```
B2_KEY_ID=...
B2_APP_KEY=...
```
