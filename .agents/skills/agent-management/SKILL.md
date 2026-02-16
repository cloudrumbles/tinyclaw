---
name: "agent-management"
description: "Create, configure, and destroy agents and teams. Use when the user asks to add a new agent, remove an agent, create a team, or modify agent/team configuration."
---

# Agent Management

You can create, modify, and destroy agents and teams by editing `~/.tinyclaw/settings.json`. Changes take effect on the next message — no restart needed.

## Creating an agent

Add a new entry to the `agents` object in `~/.tinyclaw/settings.json`:

```json
{
  "agents": {
    "existing-agent": { ... },
    "new-agent-id": {
      "name": "Human-Readable Name",
      "provider": "anthropic",
      "model": "sonnet",
      "working_directory": "new-agent-id"
    }
  }
}
```

Required fields:
- `name` — display name (e.g. "Data Analyst", "Code Reviewer")
- `provider` — `"anthropic"` (Claude) or `"openai"` (Codex)
- `model` — default to `"sonnet"` unless the user specifies otherwise. Available: `"sonnet"`, `"opus"`, `"haiku"`
- `working_directory` — use the agent ID as the directory name (relative to workspace). The directory and its CLAUDE.md are created automatically on first invocation.

Optional fields:
- `timeout` — idle timeout in seconds (default: 180)

The agent ID (the JSON key) is what users type to route messages: `@new-agent-id message here`.

## Destroying an agent

1. Remove the agent's entry from `agents` in settings.json
2. If the agent is in any team, remove it from that team's `agents` array too
3. Optionally clean up the working directory: `rm -rf <workspace>/<agent-id>/`

## Creating a team

Add a new entry to the `teams` object:

```json
{
  "teams": {
    "research": {
      "name": "Research Team",
      "agents": ["analyst", "researcher"],
      "leader_agent": "analyst"
    }
  }
}
```

Required fields:
- `name` — display name
- `agents` — array of agent IDs (must exist in `agents`)
- `leader_agent` — which agent receives messages sent to `@team-id` (must be in `agents` array)

When the user sends `@research do X`, the leader agent receives it. The leader can dispatch to teammates via `[@researcher: task description]` — teammates run asynchronously in the background.

## Destroying a team

Remove the team's entry from `teams` in settings.json. The agents themselves are NOT deleted — they just stop being part of a team.

## Modifying an agent

Edit the agent's fields in settings.json. Common changes:
- Switch model: change `"model"` to `"opus"`, `"sonnet"`, or `"haiku"`
- Change timeout: set `"timeout"` to desired seconds

## Important notes

- Agent IDs and team IDs share the same `@` namespace — don't use the same ID for both
- Always read settings.json before editing to avoid overwriting other changes
- Use `jq` or careful JSON editing — malformed JSON will break all agents
- After creating an agent, tell the user they can message it with `@agent-id`
- The working directory is auto-initialized with CLAUDE.md, skills, and soul file on first message
