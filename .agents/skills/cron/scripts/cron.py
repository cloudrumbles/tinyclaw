#!/usr/bin/env python3
"""Manage cron jobs: create/list/delete scheduled tasks via cron-job.org."""

import argparse
import json
import os
import sys
import urllib.request
import uuid
from datetime import datetime, timezone
from pathlib import Path

TINYCLAW_HOME = Path.home() / ".tinyclaw"
JOBS_FILE = TINYCLAW_HOME / "cron-jobs.json"
CONFIG_FILE = TINYCLAW_HOME / "cron-config.json"
ENV_FILE = Path.home() / ".env"

CRONJOB_API = "https://api.cron-job.org"


def load_env():
    if ENV_FILE.exists():
        for line in ENV_FILE.read_text().splitlines():
            line = line.strip()
            if line and not line.startswith("#") and "=" in line:
                key, _, value = line.partition("=")
                os.environ.setdefault(key.strip(), value.strip())


def get_api_key():
    return os.environ.get("CRONJOB_ORG_API_KEY", "")


def get_sprite_url():
    cfg = {}
    if CONFIG_FILE.exists():
        with open(CONFIG_FILE) as f:
            cfg = json.load(f)
    return cfg.get("sprite_url", "")


def load_jobs():
    if not JOBS_FILE.exists():
        return {}
    with open(JOBS_FILE) as f:
        return json.load(f).get("jobs", {})


def save_jobs(jobs):
    JOBS_FILE.parent.mkdir(parents=True, exist_ok=True)
    with open(JOBS_FILE, "w") as f:
        json.dump({"jobs": jobs}, f, indent=2)


def api_request(method, path, data=None):
    """Make a request to the cron-job.org API."""
    api_key = get_api_key()
    if not api_key:
        print("Error: CRONJOB_ORG_API_KEY not set in ~/.env", file=sys.stderr)
        sys.exit(1)

    url = f"{CRONJOB_API}{path}"
    body = json.dumps(data).encode() if data else None
    req = urllib.request.Request(url, data=body, method=method)
    req.add_header("Authorization", f"Bearer {api_key}")
    req.add_header("Content-Type", "application/json")

    try:
        resp = urllib.request.urlopen(req, timeout=15)
        if resp.status == 200:
            return json.loads(resp.read().decode())
        return {}
    except urllib.error.HTTPError as e:
        body = e.read().decode() if e.fp else ""
        print(f"API error {e.code}: {body}", file=sys.stderr)
        sys.exit(1)


def parse_cron_to_schedule(cron_expr):
    """Convert a cron expression (min hour mday month wday) to cron-job.org schedule format.

    Examples:
        '0 9 * * *'   -> 9:00 every day
        '30 14 * * 1' -> 14:30 every Monday
        '*/15 * * * *' -> every 15 minutes
        '0 9,18 * * *' -> 9:00 and 18:00 every day
    """
    parts = cron_expr.strip().split()
    if len(parts) != 5:
        print(f"Error: invalid cron expression '{cron_expr}' (need 5 fields: min hour mday month wday)", file=sys.stderr)
        sys.exit(1)

    def parse_field(field, max_val, min_val=0):
        if field == "*":
            return [-1]
        values = []
        for part in field.split(","):
            if "/" in part:
                base, step = part.split("/", 1)
                step = int(step)
                if base == "*":
                    start = min_val
                else:
                    start = int(base)
                values.extend(range(start, max_val + 1, step))
            elif "-" in part:
                lo, hi = part.split("-", 1)
                values.extend(range(int(lo), int(hi) + 1))
            else:
                values.append(int(part))
        return sorted(set(values))

    return {
        "timezone": "Asia/Singapore",
        "expiresAt": 0,
        "minutes": parse_field(parts[0], 59),
        "hours": parse_field(parts[1], 23),
        "mdays": parse_field(parts[2], 31, 1),
        "months": parse_field(parts[3], 12, 1),
        "wdays": parse_field(parts[4], 6),
    }


def cmd_create(args):
    sprite_url = get_sprite_url()
    if not sprite_url:
        print("Error: Sprite URL not configured. Run: cron.py config --sprite-url <url>", file=sys.stderr)
        sys.exit(1)

    job_id = uuid.uuid4().hex[:12]
    trigger_url = f"{sprite_url}/cron/{job_id}"

    # Register with cron-job.org
    schedule = parse_cron_to_schedule(args.schedule)
    resp = api_request("PUT", "/jobs", {
        "job": {
            "url": trigger_url,
            "enabled": True,
            "title": f"tinyclaw: {args.name}",
            "saveResponses": False,
            "requestTimeout": 30,
            "requestMethod": 0,  # GET
            "redirectSuccess": False,
            "schedule": schedule,
            "notification": {
                "onFailure": True,
                "onFailureCount": 3,
                "onSuccess": False,
                "onDisable": True,
            },
        }
    })

    cron_job_id = resp.get("jobId", 0)

    # Store locally
    jobs = load_jobs()
    jobs[job_id] = {
        "name": args.name,
        "agent_id": args.agent,
        "prompt": args.prompt,
        "chat_id": args.chat_id,
        "schedule": args.schedule,
        "recurring": args.recurring,
        "cron_job_org_id": cron_job_id,
        "trigger_url": trigger_url,
        "created_at": datetime.now(timezone.utc).isoformat(),
    }
    save_jobs(jobs)

    mode = "recurring" if args.recurring else "one-shot (auto-deletes after firing)"
    print(f"Created job: {job_id}")
    print(f"  Name: {args.name}")
    print(f"  Schedule: {args.schedule}")
    print(f"  Mode: {mode}")
    print(f"  Agent: {args.agent}")
    print(f"  Trigger URL: {trigger_url}")
    print(f"  cron-job.org ID: {cron_job_id}")


def cmd_list(args):
    jobs = load_jobs()
    if not jobs:
        print("No cron jobs configured.")
        return
    for jid, job in jobs.items():
        mode = "recurring" if job.get("recurring", False) else "one-shot"
        print(f"  {jid}: {job['name']} [{mode}]")
        print(f"    Schedule: {job.get('schedule', '?')}")
        print(f"    Agent: {job.get('agent_id', '?')}")
        print(f"    Prompt: {job.get('prompt', '?')[:80]}")
        print(f"    Chat ID: {job.get('chat_id', '?')}")
        print(f"    cron-job.org ID: {job.get('cron_job_org_id', '?')}")
        print()


def cmd_delete(args):
    jobs = load_jobs()
    if args.job_id not in jobs:
        print(f"Job {args.job_id} not found.", file=sys.stderr)
        sys.exit(1)

    job = jobs[args.job_id]

    # Delete from cron-job.org
    cron_id = job.get("cron_job_org_id")
    if cron_id:
        try:
            api_request("DELETE", f"/jobs/{cron_id}")
            print(f"Deleted from cron-job.org (ID: {cron_id})")
        except Exception as e:
            print(f"Warning: failed to delete from cron-job.org: {e}", file=sys.stderr)

    del jobs[args.job_id]
    save_jobs(jobs)
    print(f"Deleted job: {args.job_id}")


def cmd_config(args):
    cfg = {}
    if CONFIG_FILE.exists():
        with open(CONFIG_FILE) as f:
            cfg = json.load(f)

    if args.sprite_url:
        cfg["sprite_url"] = args.sprite_url.rstrip("/")
        CONFIG_FILE.parent.mkdir(parents=True, exist_ok=True)
        with open(CONFIG_FILE, "w") as f:
            json.dump(cfg, f, indent=2)
        print(f"Sprite URL set to: {cfg['sprite_url']}")
    else:
        print(f"Sprite URL: {cfg.get('sprite_url', '(not set)')}")


def cmd_trigger(args):
    """Manually trigger a job (for testing)."""
    jobs = load_jobs()
    if args.job_id not in jobs:
        print(f"Job {args.job_id} not found.", file=sys.stderr)
        sys.exit(1)
    job = jobs[args.job_id]
    trigger_url = job.get("trigger_url", "")
    if trigger_url:
        print(f"Trigger URL: {trigger_url}")
        print(f"curl {trigger_url}")
    else:
        print(f"curl http://localhost:8080/cron/{args.job_id}")


def main():
    load_env()

    parser = argparse.ArgumentParser(description="Manage TinyClaw cron jobs")
    sub = parser.add_subparsers(dest="command")

    p_create = sub.add_parser("create", help="Create a new cron job")
    p_create.add_argument("--name", required=True, help="Job name")
    p_create.add_argument("--prompt", required=True, help="Prompt to send to the agent")
    p_create.add_argument("--schedule", required=True, help="Cron expression (min hour mday month wday)")
    p_create.add_argument("--agent", default="sultana", help="Agent ID (default: sultana)")
    p_create.add_argument("--chat-id", default="525365593", help="Telegram chat ID for notifications")
    p_create.add_argument("--recurring", action="store_true", help="Keep job after firing (default: one-shot, auto-deletes)")

    sub.add_parser("list", help="List all cron jobs")

    p_delete = sub.add_parser("delete", help="Delete a cron job")
    p_delete.add_argument("job_id", help="Job ID to delete")

    p_config = sub.add_parser("config", help="Configure cron settings")
    p_config.add_argument("--sprite-url", help="Set the Sprite public URL")

    p_trigger = sub.add_parser("trigger", help="Show trigger URL for a job")
    p_trigger.add_argument("job_id", help="Job ID")

    args = parser.parse_args()

    if args.command == "create":
        cmd_create(args)
    elif args.command == "list":
        cmd_list(args)
    elif args.command == "delete":
        cmd_delete(args)
    elif args.command == "config":
        cmd_config(args)
    elif args.command == "trigger":
        cmd_trigger(args)
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
