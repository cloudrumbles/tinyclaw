---
name: cron
description: Schedule recurring or one-shot tasks using cron-job.org. Use when asked to set a reminder, schedule something, run a task at a specific time, create a recurring job, or anything involving "cron", "schedule", "remind me", "every day", "every hour", "at 9am", "timer".
---

# Cron Jobs

Schedule tasks by registering them with cron-job.org, which sends HTTP pings to TinyClaw's `/cron/{job_id}` endpoint.

## Webhook URL

```
https://163967bd9d1bd2f692a45a5a69885a9a.preview.bl.run/cron/{job_id}
```

## How It Works

1. You create a job entry in `~/sultana-workspace/cron-jobs.json`
2. You register the job with cron-job.org via their API
3. At the scheduled time, cron-job.org sends GET to the webhook URL
4. TinyClaw looks up the job_id, reads the prompt, and injects it into your message queue
5. You receive the prompt as if it were a regular message and act on it

## cron-jobs.json Format

```json
{
  "jobs": {
    "some_unique_id": {
      "name": "Daily Check-in",
      "prompt": "Good morning! Review today's calendar and tasks.",
      "chat_id": 525365593,
      "recurring": true,
      "cron_job_org_id": 12345678
    }
  }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `name` | string | Human-readable job name |
| `prompt` | string | The message injected into your queue when triggered |
| `chat_id` | int | Telegram chat ID to send responses to (Shah: `525365593`) |
| `recurring` | bool | `true` = keeps running, `false` = auto-deletes after firing once |
| `cron_job_org_id` | int | The ID returned by cron-job.org API (for deletion later) |

## cron-job.org API

API key is in `~/.tinyclaw/sultana/keys.env` as `CRONJOB_ORG_API_KEY`.

### Create a job

```bash
curl -s -X PUT "https://api.cron-job.org/jobs" \
  -H "Authorization: Bearer $CRONJOB_ORG_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "job": {
      "url": "https://163967bd9d1bd2f692a45a5a69885a9a.preview.bl.run/cron/YOUR_JOB_ID",
      "title": "Job Name",
      "enabled": true,
      "saveResponses": false,
      "schedule": {
        "timezone": "Asia/Singapore",
        "hours": [9],
        "mdays": [-1],
        "minutes": [0],
        "months": [-1],
        "wdays": [-1]
      }
    }
  }'
```

Schedule fields use `-1` to mean "every". Examples:
- **Every day at 9:00 SGT**: `hours: [9], minutes: [0], mdays: [-1], months: [-1], wdays: [-1]`
- **Every hour**: `hours: [-1], minutes: [0]`
- **Weekdays at 8:30**: `hours: [8], minutes: [30], wdays: [1,2,3,4,5]`
- **Once** (use a near-future time, set `recurring: false` in cron-jobs.json)

The API returns `{"jobId": 12345678}` — save this as `cron_job_org_id` in your jobs file.

### Delete a job

```bash
curl -s -X DELETE "https://api.cron-job.org/jobs/$CRON_JOB_ORG_ID" \
  -H "Authorization: Bearer $CRONJOB_ORG_API_KEY"
```

### List jobs

```bash
curl -s "https://api.cron-job.org/jobs" \
  -H "Authorization: Bearer $CRONJOB_ORG_API_KEY"
```

## Workflow

### Creating a scheduled task

1. Generate a unique job ID (e.g. UUID or short random hex)
2. Add entry to `~/sultana-workspace/cron-jobs.json`
3. Call cron-job.org API to create the job
4. Save the returned `jobId` back into the jobs file as `cron_job_org_id`

### Deleting a scheduled task

1. Read the `cron_job_org_id` from `cron-jobs.json`
2. Call cron-job.org DELETE API
3. Remove the entry from `cron-jobs.json`
