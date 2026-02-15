#!/usr/bin/env python3
"""HTTP server that receives cron triggers and writes to TinyClaw's cron inbox."""

import json
import os
import sys
import time
import threading
import urllib.request
from http.server import HTTPServer, BaseHTTPRequestHandler
from pathlib import Path

TINYCLAW_HOME = Path.home() / ".tinyclaw"
JOBS_FILE = TINYCLAW_HOME / "cron-jobs.json"
CRON_INBOX = TINYCLAW_HOME / "cron-inbox"
ENV_FILE = Path.home() / ".env"


def load_env():
    if ENV_FILE.exists():
        for line in ENV_FILE.read_text().splitlines():
            line = line.strip()
            if line and not line.startswith("#") and "=" in line:
                key, _, value = line.partition("=")
                os.environ.setdefault(key.strip(), value.strip())


def load_jobs():
    if not JOBS_FILE.exists():
        return {}
    with open(JOBS_FILE) as f:
        return json.load(f).get("jobs", {})


def save_jobs(jobs):
    with open(JOBS_FILE, "w") as f:
        json.dump({"jobs": jobs}, f, indent=2)


def delete_from_cronjob_org(cron_id):
    api_key = os.environ.get("CRONJOB_ORG_API_KEY", "")
    if not cron_id or not api_key:
        return
    try:
        req = urllib.request.Request(
            f"https://api.cron-job.org/jobs/{cron_id}",
            method="DELETE",
        )
        req.add_header("Authorization", f"Bearer {api_key}")
        urllib.request.urlopen(req, timeout=10)
        print(f"[cron] Deleted from cron-job.org (ID: {cron_id})", file=sys.stderr)
    except Exception as e:
        print(f"[cron] Failed to delete from cron-job.org: {e}", file=sys.stderr)


def trigger_job(job_id, job):
    """Write a trigger file to TinyClaw's cron inbox."""
    CRON_INBOX.mkdir(parents=True, exist_ok=True)

    chat_id = job.get("chat_id")
    # Convert chat_id to int if it's a string
    if isinstance(chat_id, str) and chat_id.lstrip("-").isdigit():
        chat_id = int(chat_id)

    trigger = {
        "job_id": job_id,
        "name": job.get("name", "Cron Job"),
        "agent_id": job.get("agent_id", "sultana"),
        "prompt": job.get("prompt", ""),
        "chat_id": chat_id,
    }

    filename = f"cron_{job_id}_{int(time.time() * 1000)}.json"
    trigger_path = CRON_INBOX / filename

    with open(trigger_path, "w") as f:
        json.dump(trigger, f)

    print(f"[cron] Triggered job {job_id}: wrote {filename}", file=sys.stderr)

    # Auto-delete one-shot jobs
    if not job.get("recurring", False):
        print(f"[cron] One-shot job {job_id}, auto-deleting", file=sys.stderr)
        jobs = load_jobs()
        cron_id = job.get("cron_job_org_id")
        if job_id in jobs:
            del jobs[job_id]
            save_jobs(jobs)
        delete_from_cronjob_org(cron_id)


class CronHandler(BaseHTTPRequestHandler):
    def do_GET(self):
        path = self.path.rstrip("/")

        if path == "/health":
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.end_headers()
            self.wfile.write(b"ok")
            return

        if path.startswith("/cron/"):
            job_id = path[6:]
            jobs = load_jobs()

            if job_id not in jobs:
                self.send_response(404)
                self.send_header("Content-Type", "text/plain")
                self.end_headers()
                self.wfile.write(b"job not found")
                return

            job = jobs[job_id]
            threading.Thread(target=trigger_job, args=(job_id, job), daemon=True).start()

            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.end_headers()
            self.wfile.write(b"ok")
            return

        self.send_response(404)
        self.send_header("Content-Type", "text/plain")
        self.end_headers()
        self.wfile.write(b"not found")

    do_POST = do_GET

    def log_message(self, format, *args):
        print(f"[cron-server] {args[0]}", file=sys.stderr)


if __name__ == "__main__":
    load_env()
    port = int(os.environ.get("CRON_PORT", "8080"))
    CRON_INBOX.mkdir(parents=True, exist_ok=True)
    server = HTTPServer(("0.0.0.0", port), CronHandler)
    print(f"[cron-server] Listening on port {port}", file=sys.stderr)
    server.serve_forever()
