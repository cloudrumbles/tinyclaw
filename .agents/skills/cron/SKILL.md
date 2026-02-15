---
name: cron
description: Schedule recurring tasks using cron jobs. Use this skill when users want to schedule something, set reminders, run tasks at specific times, or manage recurring jobs. Triggers on phrases like "schedule", "remind me at", "every day at", "run at", "cron", "recurring", "timer", "alarm", "wake up at".
---

# Cron Job Manager

Schedule tasks that run even when the Sprite is asleep. Jobs are stored locally; cron-job.org sends HTTP pings to wake the Sprite at the right time.

**How it works**: When a cron job fires, the prompt is injected into your active conversation session — the same one as Telegram messages. You will receive it as a `[CRON JOB: <name>]` message. Process it and respond normally — your response will be sent to the user's Telegram chat automatically.

## Usage

```bash
python3 ~/.agents/skills/cron/scripts/cron.py <command> [options]
```

### Create a job

```bash
# One-shot (default): fires once then auto-deletes
python3 ~/.agents/skills/cron/scripts/cron.py create \
  --name "Reminder" \
  --prompt "Hey! Time for your meeting." \
  --schedule "0 14 15 2 *" \
  --agent sultana \
  --chat-id 525365593

# Recurring: keeps firing on schedule
python3 ~/.agents/skills/cron/scripts/cron.py create \
  --name "Morning check-in" \
  --prompt "Good morning! Review pending tasks and give a status update." \
  --schedule "0 9 * * *" \
  --agent sultana \
  --chat-id 525365593 \
  --recurring
```

**Default behavior**: Jobs are **one-shot** — they fire once, then auto-delete. Use `--recurring` to keep a job running on its schedule.

**Schedule format**: Standard 5-field cron (`minute hour day-of-month month day-of-week`). All times are **SGT** (Asia/Singapore, UTC+8).

Examples:
- `0 9 * * *` — every day at 9:00 SGT
- `0 9 * * 1-5` — weekdays at 9:00 SGT
- `30 14 * * *` — every day at 14:30 SGT
- `0 */6 * * *` — every 6 hours
- `*/30 * * * *` — every 30 minutes

### List jobs

```bash
python3 ~/.agents/skills/cron/scripts/cron.py list
```

### Delete a job

```bash
python3 ~/.agents/skills/cron/scripts/cron.py delete <job_id>
```

## Important Notes

- All times are **SGT** (UTC+8)
- The `--chat-id` is the Telegram chat ID where your response will appear
- The `--agent` must match an agent in `~/.tinyclaw/settings.json`
- The `--prompt` is what you (the agent) will receive — write it as instructions to yourself
- Jobs named "Heartbeat" are special: they route through the heartbeat module which includes system vitals
