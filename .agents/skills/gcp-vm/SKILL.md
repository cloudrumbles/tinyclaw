---
name: gcp-vm
description: Access a persistent Linux VM on Google Cloud. Use this skill when you need to run long processes, install heavy tools, store large files, compile code, run servers, or do anything that requires more disk or a persistent environment. Triggers on phrases like "run on the vm", "ssh", "server", "install on remote", "persistent storage", "compile", "build", "download large file".
---

# GCP VM Access

You have SSH access to a persistent Linux VM on Google Cloud (free tier).

## Quick Reference

| Field | Value |
|-------|-------|
| Host alias | `sultana-vm` |
| IP | `104.154.140.203` |
| User | `tinyclaw` |
| Zone | `us-central1-a` |
| Specs | e2-micro: 0.25 vCPU (shared), 1GB RAM, 30GB disk |
| OS | Ubuntu 24.04 LTS |

## Usage

```bash
# Run a single command
ssh sultana-vm 'uname -a'

# Interactive session
ssh sultana-vm

# Copy files to the VM
scp -F ~/.ssh/config /path/to/local/file sultana-vm:/path/on/vm/

# Copy files from the VM
scp -F ~/.ssh/config sultana-vm:/path/on/vm/file /path/to/local/
```

The SSH config is already set up at `~/.ssh/config` — just use the `sultana-vm` host alias.

## When to Use This

- **Persistent storage**: The VM has a real 30GB SSD disk (unlike our Blaxel sandbox which uses tmpfs)
- **Heavy installs**: Tools that are too large for our sandbox (e.g. compilers, databases, language runtimes)
- **Long-running processes**: Background jobs, downloads, builds that take a while
- **Cron jobs or daemons**: Anything that needs to keep running independently

## Important Notes

- This is a **free tier** instance — 0.25 vCPU is slow. Don't expect fast compilation.
- The VM may be stopped to save resources. If SSH fails with "connection refused", let Shah know so he can start it.
- You have full sudo access on the VM (`sudo` without password for the `tinyclaw` user — run `sudo <command>` if you need root).
- Files on this VM persist across reboots. This is real disk, not tmpfs.
