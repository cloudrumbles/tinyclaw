use std::collections::HashMap;
use std::path::Path;

use crate::types::{AgentConfig, TeamConfig};

// Template files embedded at compile time
const AGENTS_MD: &str = include_str!("../templates/AGENTS.md");
const SOUL_MD: &str = include_str!("../templates/SOUL.md");
const HEARTBEAT_MD: &str = include_str!("../templates/heartbeat.md");

/// Ensure agent directory exists with template files.
/// For embedded templates, we write them from the compiled-in strings.
/// For the skills directory, we symlink from the project's .agents/skills.
pub fn ensure_agent_directory(agent_dir: &Path, skills_source: Option<&Path>) {
    if agent_dir.exists() {
        return;
    }

    std::fs::create_dir_all(agent_dir).ok();

    // Write AGENTS.md
    let agents_md_path = agent_dir.join("AGENTS.md");
    std::fs::write(&agents_md_path, AGENTS_MD).ok();

    // Write .claude/CLAUDE.md (same content as AGENTS.md)
    let claude_dir = agent_dir.join(".claude");
    std::fs::create_dir_all(&claude_dir).ok();
    std::fs::write(claude_dir.join("CLAUDE.md"), AGENTS_MD).ok();

    // Write heartbeat.md
    std::fs::write(agent_dir.join("heartbeat.md"), HEARTBEAT_MD).ok();

    // Write .tinyclaw/SOUL.md
    let tinyclaw_dir = agent_dir.join(".tinyclaw");
    std::fs::create_dir_all(&tinyclaw_dir).ok();
    std::fs::write(tinyclaw_dir.join("SOUL.md"), SOUL_MD).ok();

    // Symlink skills directory if source exists
    if let Some(skills_src) = skills_source {
        if skills_src.exists() {
            let claude_skills = claude_dir.join("skills");
            if !claude_skills.exists() {
                #[cfg(unix)]
                std::os::unix::fs::symlink(skills_src, &claude_skills).ok();
            }

            let agent_skills_dir = agent_dir.join(".agent");
            std::fs::create_dir_all(&agent_skills_dir).ok();
            let agent_skills = agent_skills_dir.join("skills");
            if !agent_skills.exists() {
                #[cfg(unix)]
                std::os::unix::fs::symlink(skills_src, &agent_skills).ok();
            }
        }
    }
}

/// Update AGENTS.md with current teammate info.
/// Replaces content between `<!-- TEAMMATES_START -->` and `<!-- TEAMMATES_END -->`.
pub fn update_agent_teammates(
    agent_dir: &Path,
    agent_id: &str,
    agents: &HashMap<String, AgentConfig>,
    teams: &HashMap<String, TeamConfig>,
) {
    let agents_md_path = agent_dir.join("AGENTS.md");
    let Ok(content) = std::fs::read_to_string(&agents_md_path) else {
        return;
    };

    let start_marker = "<!-- TEAMMATES_START -->";
    let end_marker = "<!-- TEAMMATES_END -->";

    let Some(start_idx) = content.find(start_marker) else {
        return;
    };
    let Some(end_idx) = content.find(end_marker) else {
        return;
    };

    // Find teammates from all teams this agent belongs to
    let mut teammates: Vec<(&str, &str, &str)> = Vec::new(); // (id, name, model)
    for team in teams.values() {
        if !team.agents.iter().any(|a| a == agent_id) {
            continue;
        }
        for tid in &team.agents {
            if tid == agent_id {
                continue;
            }
            if let Some(agent) = agents.get(tid.as_str()) {
                if !teammates.iter().any(|(id, _, _)| *id == tid.as_str()) {
                    teammates.push((tid, &agent.name, &agent.model));
                }
            }
        }
    }

    let mut block = String::new();
    if let Some(self_agent) = agents.get(agent_id) {
        block.push_str(&format!(
            "\n### You\n\n- `@{agent_id}` — **{}** ({})\n",
            self_agent.name, self_agent.model
        ));
    }
    if !teammates.is_empty() {
        block.push_str("\n### Your Teammates\n\n");
        for (id, name, model) in &teammates {
            block.push_str(&format!("- `@{id}` — **{name}** ({model})\n"));
        }
    }

    let new_content = format!(
        "{}{}{}{}",
        &content[..start_idx + start_marker.len()],
        block,
        &content[end_idx..],
        "" // just to avoid trailing
    );
    std::fs::write(&agents_md_path, &new_content).ok();

    // Also write to .claude/CLAUDE.md
    let claude_dir = agent_dir.join(".claude");
    std::fs::create_dir_all(&claude_dir).ok();
    let claude_md_path = claude_dir.join("CLAUDE.md");

    let claude_content = std::fs::read_to_string(&claude_md_path).unwrap_or_default();
    let claude_start = claude_content.find(start_marker);
    let claude_end = claude_content.find(end_marker);

    let new_claude = if let (Some(cs), Some(ce)) = (claude_start, claude_end) {
        format!(
            "{}{}{}",
            &claude_content[..cs + start_marker.len()],
            block,
            &claude_content[ce..]
        )
    } else {
        format!(
            "{}\n\n{start_marker}{block}{end_marker}\n",
            claude_content.trim_end()
        )
    };
    std::fs::write(&claude_md_path, new_claude).ok();
}
