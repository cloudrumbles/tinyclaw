"""
Backup/restore TinyClaw infrastructure to Backblaze B2.

Usage:
    python3 backup.py infra            # backup infrastructure (excludes persona)
    python3 backup.py list             # list remote backups
    python3 backup.py restore <key>    # restore a specific backup

Reads B2 credentials from ~/.tinyclaw/sultana/keys.env
Persona is backed up via git, not this script.
"""

import base64
import hashlib
import json
import sys
import tarfile
import tempfile
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

HOME = Path.home()
TINYCLAW_HOME = HOME / ".tinyclaw"
WORKSPACE_DIR = HOME / "sultana-workspace"
KEYS_FILE = TINYCLAW_HOME / "sultana" / "keys.env"
BUCKET_ID = "bf6aa1c0a17b6d5997ce071b"
BUCKET_NAME = "sultana-backups"


def load_keys() -> tuple[str, str]:
    if not KEYS_FILE.exists():
        print(f"ERROR: keys file not found: {KEYS_FILE}", file=sys.stderr)
        sys.exit(1)
    keys = {}
    for line in KEYS_FILE.read_text().splitlines():
        line = line.strip()
        if "=" in line and not line.startswith("#"):
            k, v = line.split("=", 1)
            keys[k.strip()] = v.strip()
    key_id = keys.get("B2_KEY_ID", "")
    app_key = keys.get("B2_APP_KEY", "")
    if not key_id or not app_key:
        print("ERROR: B2_KEY_ID or B2_APP_KEY missing from keys.env", file=sys.stderr)
        sys.exit(1)
    return key_id, app_key


def b2_authorize(key_id: str, app_key: str) -> tuple[str, str]:
    creds = base64.b64encode(f"{key_id}:{app_key}".encode()).decode()
    req = urllib.request.Request(
        "https://api.backblazeb2.com/b2api/v3/b2_authorize_account",
        headers={"Authorization": f"Basic {creds}"},
    )
    with urllib.request.urlopen(req) as resp:
        data = json.loads(resp.read())
    return data["apiInfo"]["storageApi"]["apiUrl"], data["authorizationToken"]


def b2_get_upload_url(api_url: str, token: str) -> tuple[str, str]:
    req = urllib.request.Request(
        f"{api_url}/b2api/v3/b2_get_upload_url",
        data=json.dumps({"bucketId": BUCKET_ID}).encode(),
        headers={"Authorization": token, "Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req) as resp:
        data = json.loads(resp.read())
    return data["uploadUrl"], data["authorizationToken"]


def b2_upload(upload_url: str, upload_token: str, file_path: Path, remote_name: str):
    file_data = file_path.read_bytes()
    sha1 = hashlib.sha1(file_data).hexdigest()
    req = urllib.request.Request(
        upload_url,
        data=file_data,
        headers={
            "Authorization": upload_token,
            "X-Bz-File-Name": remote_name,
            "Content-Type": "application/gzip",
            "Content-Length": str(len(file_data)),
            "X-Bz-Content-Sha1": sha1,
        },
        method="POST",
    )
    with urllib.request.urlopen(req) as resp:
        data = json.loads(resp.read())
    print(f"  uploaded: {data['fileName']} ({len(file_data) / 1024 / 1024:.1f}MB)")


def b2_list_files(api_url: str, token: str) -> list[dict]:
    req = urllib.request.Request(
        f"{api_url}/b2api/v3/b2_list_file_names",
        data=json.dumps({"bucketId": BUCKET_ID, "maxFileCount": 100}).encode(),
        headers={"Authorization": token, "Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req) as resp:
        data = json.loads(resp.read())
    return data.get("files", [])


def b2_download(api_url: str, token: str, file_name: str, dest: Path):
    url = f"{api_url}/file/{BUCKET_NAME}/{file_name}"
    req = urllib.request.Request(url, headers={"Authorization": token})
    with urllib.request.urlopen(req) as resp:
        dest.write_bytes(resp.read())


def backup_infra(api_url: str, token: str):
    """Backup tinyclaw infra (excluding persona and logs) to B2."""
    print("Backing up infrastructure...")
    ts = datetime.now(timezone.utc).strftime("%Y-%m-%d-%H%M%S")
    remote_name = f"infra/tinyclaw-{ts}.tar.gz"

    with tempfile.NamedTemporaryFile(suffix=".tar.gz", delete=False) as tmp:
        tmp_path = Path(tmp.name)
    try:
        with tarfile.open(tmp_path, "w:gz") as tar:
            if TINYCLAW_HOME.exists():
                for item in sorted(TINYCLAW_HOME.rglob("*")):
                    rel = item.relative_to(TINYCLAW_HOME)
                    if any(part in ("sultana", "logs") for part in rel.parts):
                        continue
                    tar.add(item, arcname=f"tinyclaw-home/{rel}", recursive=False)

            if WORKSPACE_DIR.exists():
                for item in sorted(WORKSPACE_DIR.rglob("*")):
                    rel = item.relative_to(WORKSPACE_DIR)
                    if "logs" in rel.parts:
                        continue
                    tar.add(item, arcname=f"sultana-workspace/{rel}", recursive=False)

        size_mb = tmp_path.stat().st_size / 1024 / 1024
        print(f"  archive: {size_mb:.1f}MB")
        upload_url, upload_token = b2_get_upload_url(api_url, token)
        b2_upload(upload_url, upload_token, tmp_path, remote_name)
    finally:
        tmp_path.unlink(missing_ok=True)


def list_backups(api_url: str, token: str):
    files = b2_list_files(api_url, token)
    if not files:
        print("No backups found.")
        return
    for f in files:
        size_mb = f["contentLength"] / 1024 / 1024
        print(f"  {f['fileName']}  ({size_mb:.1f}MB)")


def restore(api_url: str, token: str, key: str):
    print(f"Restoring {key}...")
    with tempfile.NamedTemporaryFile(suffix=".tar.gz", delete=False) as tmp:
        tmp_path = Path(tmp.name)
    try:
        b2_download(api_url, token, key, tmp_path)
        size_mb = tmp_path.stat().st_size / 1024 / 1024
        print(f"  downloaded: {size_mb:.1f}MB")

        with tarfile.open(tmp_path, "r:gz") as tar:
            for member in tar.getmembers():
                if member.name.startswith("tinyclaw-home/"):
                    member.name = member.name[len("tinyclaw-home/"):]
                    tar.extract(member, TINYCLAW_HOME)
                elif member.name.startswith("sultana-workspace/"):
                    member.name = member.name[len("sultana-workspace/"):]
                    tar.extract(member, WORKSPACE_DIR)
        print(f"  extracted to {TINYCLAW_HOME} and {WORKSPACE_DIR}")
    finally:
        tmp_path.unlink(missing_ok=True)


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        sys.exit(1)

    cmd = sys.argv[1]
    key_id, app_key = load_keys()
    api_url, token = b2_authorize(key_id, app_key)

    if cmd == "infra":
        backup_infra(api_url, token)
    elif cmd == "list":
        list_backups(api_url, token)
    elif cmd == "restore":
        if len(sys.argv) < 3:
            print("Usage: backup.py restore <key>", file=sys.stderr)
            sys.exit(1)
        restore(api_url, token, sys.argv[2])
    else:
        print(f"Unknown command: {cmd}", file=sys.stderr)
        sys.exit(1)

    print("Done.")


if __name__ == "__main__":
    main()
