use std::collections::HashMap;
use std::path::Path;

use tracing::info;

use crate::config;
use crate::types::{AgentConfig, TeamConfig};

// Template files embedded at compile time
const AGENTS_MD: &str = include_str!("../templates/AGENTS.md");
const SOUL_MD: &str = include_str!("../templates/SOUL.md");
const HEARTBEAT_MD: &str = include_str!("../templates/heartbeat.md");

/// Ensure persona directory exists with soul template and empty subdirs.
/// Handles migration from old flat layout (.tinyclaw/SOUL.md in workspace).
pub fn ensure_persona(
    tinyclaw_home: &Path,
    persona_id: &str,
    workspace_root: &Path,
    agent_id: &str,
) {
    let persona = config::persona_dir(tinyclaw_home, persona_id);
    if persona.exists() {
        return;
    }

    std::fs::create_dir_all(&persona).ok();
    std::fs::create_dir_all(persona.join("skills")).ok();
    std::fs::create_dir_all(persona.join("claude-state")).ok();

    // Migration: move old SOUL.md from workspace if it exists
    let old_soul = workspace_root.join(agent_id).join(".tinyclaw/SOUL.md");
    if old_soul.exists() {
        if std::fs::copy(&old_soul, persona.join("soul.md")).is_ok() {
            info!("Migrated soul.md from old layout for persona {persona_id}");
        }
    } else {
        std::fs::write(persona.join("soul.md"), SOUL_MD).ok();
    }

    // Migration: move old Claude CLI session data if it exists
    let claude_home = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".claude/projects");
    let old_flat_dir = workspace_root.join(agent_id);
    if old_flat_dir.exists() {
        let old_hash = config::claude_project_hash(&old_flat_dir);
        let old_session_dir = claude_home.join(&old_hash);
        if old_session_dir.exists() && !old_session_dir.is_symlink() {
            // Move contents into persona's claude-state
            let claude_state = persona.join("claude-state");
            if let Ok(entries) = std::fs::read_dir(&old_session_dir) {
                for entry in entries.flatten() {
                    let dest = claude_state.join(entry.file_name());
                    if entry.path().is_dir() {
                        // For directories like memory/, copy recursively
                        copy_dir_recursive(&entry.path(), &dest);
                    } else {
                        std::fs::copy(entry.path(), &dest).ok();
                    }
                }
            }
            info!("Migrated Claude session data for persona {persona_id}");
        }
    }

    info!("Initialized persona: {persona_id}");
}

/// Ensure workspace directory exists with infrastructure scaffolding.
/// Handles migration from old flat layout (no _meta/ dir).
pub fn ensure_workspace(
    workspace_root: &Path,
    agent_id: &str,
    ws_name: &str,
    skills_source: Option<&Path>,
) {
    let agent_root = workspace_root.join(agent_id);
    let ws_dir = config::agent_workspace_dir(workspace_root, agent_id, ws_name);

    // Migration: if agent_root exists but has no _meta/, it's the old flat layout
    if agent_root.exists() && !agent_root.join("_meta").exists() {
        migrate_flat_workspace(&agent_root, ws_name);
    }

    if ws_dir.exists() {
        return;
    }

    std::fs::create_dir_all(&ws_dir).ok();

    // Write infrastructure files
    std::fs::write(ws_dir.join("AGENTS.md"), AGENTS_MD).ok();
    std::fs::write(ws_dir.join("heartbeat.md"), HEARTBEAT_MD).ok();

    // Create .claude dir (CLAUDE.md is assembled at invocation time)
    let claude_dir = ws_dir.join(".claude");
    std::fs::create_dir_all(&claude_dir).ok();

    // Symlink infrastructure skills
    if let Some(skills_src) = skills_source {
        if skills_src.exists() {
            let claude_skills = claude_dir.join("skills");
            if !claude_skills.exists() {
                #[cfg(unix)]
                std::os::unix::fs::symlink(skills_src, &claude_skills).ok();
            }
        }
    }

    // Write active workspace marker
    let meta = agent_root.join("_meta");
    std::fs::create_dir_all(&meta).ok();
    std::fs::create_dir_all(meta.join("backups")).ok();
    if !meta.join("active").exists() {
        std::fs::write(meta.join("active"), ws_name).ok();
    }

    info!("Initialized workspace: {agent_id}/{ws_name}");
}

/// Assemble .claude/CLAUDE.md by combining infrastructure + persona + teammates.
/// Also sets up the Claude CLI session symlink for conversation continuity.
/// Called on every invocation (not just first run).
pub fn assemble_claude_md(
    workspace_dir: &Path,
    tinyclaw_home: &Path,
    persona_id: &str,
    agent_id: &str,
    agents: &HashMap<String, AgentConfig>,
    teams: &HashMap<String, TeamConfig>,
    infra_skills: Option<&Path>,
) {
    let persona = config::persona_dir(tinyclaw_home, persona_id);
    let mut assembled = String::new();

    // Layer 1: Infrastructure template
    assembled.push_str(AGENTS_MD);

    // Layer 2: Inject persona soul
    if let Ok(soul) = std::fs::read_to_string(persona.join("soul.md")) {
        let trimmed = soul.trim();
        if !trimmed.is_empty() {
            // Replace the PERSONA markers with soul content
            let persona_start = "<!-- PERSONA_START -->";
            let persona_end = "<!-- PERSONA_END -->";
            if let (Some(si), Some(ei)) = (assembled.find(persona_start), assembled.find(persona_end)) {
                let persona_section = format!(
                    "\n## Soul\n\n{trimmed}\n\n\
                     ### Persona Files\n\n\
                     Your identity and memories are stored outside this workspace so they persist across workspace swaps:\n\n\
                     - **Soul**: `{soul_path}`\n\
                     - **Skills you created**: `{skills_path}`\n\n\
                     Update your soul file as you develop opinions, expertise, and personality.\n",
                    soul_path = persona.join("soul.md").display(),
                    skills_path = persona.join("skills").display(),
                );
                assembled = format!(
                    "{}{}{}",
                    &assembled[..si + persona_start.len()],
                    persona_section,
                    &assembled[ei..],
                );
            }
        }
    }

    // Inject teammate info
    let teammates_start = "<!-- TEAMMATES_START -->";
    let teammates_end = "<!-- TEAMMATES_END -->";
    if let (Some(si), Some(ei)) = (assembled.find(teammates_start), assembled.find(teammates_end)) {
        let block = build_teammates_block(agent_id, agents, teams);
        assembled = format!(
            "{}{}{}",
            &assembled[..si + teammates_start.len()],
            block,
            &assembled[ei..],
        );
    }

    // Write assembled CLAUDE.md
    let claude_dir = workspace_dir.join(".claude");
    std::fs::create_dir_all(&claude_dir).ok();
    std::fs::write(claude_dir.join("CLAUDE.md"), &assembled).ok();

    // Also write AGENTS.md for legacy compat
    std::fs::write(workspace_dir.join("AGENTS.md"), &assembled).ok();

    // Merge skills: infrastructure + persona
    setup_skills(&claude_dir, infra_skills, &persona.join("skills"));

    // Set up Claude CLI session symlink for conversation continuity
    setup_session_symlink(workspace_dir, &persona.join("claude-state"));
}

/// Build the teammates info block for injection into AGENTS.md.
fn build_teammates_block(
    agent_id: &str,
    agents: &HashMap<String, AgentConfig>,
    teams: &HashMap<String, TeamConfig>,
) -> String {
    let mut teammates: Vec<(&str, &str, &str)> = Vec::new();
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
    block
}

/// Set up skills directory by symlinking individual skill dirs from both
/// infrastructure and persona sources. Each skill becomes its own symlink
/// so both sources are merged into one flat directory.
fn setup_skills(claude_dir: &Path, infra_skills: Option<&Path>, persona_skills: &Path) {
    let skills_dir = claude_dir.join("skills");

    // If skills is already a single symlink (old layout), remove it first
    if skills_dir.is_symlink() {
        std::fs::remove_file(&skills_dir).ok();
    }

    std::fs::create_dir_all(&skills_dir).ok();

    // Symlink individual infrastructure skills
    if let Some(infra) = infra_skills {
        symlink_skill_entries(infra, &skills_dir);
    }

    // Symlink individual persona skills (agent-created)
    if persona_skills.exists() {
        symlink_skill_entries(persona_skills, &skills_dir);
    }
}

/// Symlink each subdirectory from source into target.
fn symlink_skill_entries(source: &Path, target: &Path) {
    let Ok(entries) = std::fs::read_dir(source) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.path().is_dir() {
            let link = target.join(entry.file_name());
            if !link.exists() {
                #[cfg(unix)]
                std::os::unix::fs::symlink(entry.path(), &link).ok();
            }
        }
    }
}

/// Create/update the Claude CLI project directory symlink so that session data
/// lives in the persona directory, enabling conversation continuity across
/// workspace swaps and machine migrations.
fn setup_session_symlink(workspace_dir: &Path, claude_state: &Path) {
    std::fs::create_dir_all(claude_state).ok();

    let claude_home = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".claude/projects");
    std::fs::create_dir_all(&claude_home).ok();

    let hash = config::claude_project_hash(workspace_dir);
    let project_dir = claude_home.join(&hash);

    // If it's already a symlink pointing to the right place, we're done
    if project_dir.is_symlink() {
        if let Ok(target) = std::fs::read_link(&project_dir) {
            if target == claude_state {
                return;
            }
        }
        // Wrong target — remove and recreate
        std::fs::remove_file(&project_dir).ok();
    }

    // If it's a real directory (not symlink), move contents to claude-state first
    if project_dir.exists() && !project_dir.is_symlink() {
        if let Ok(entries) = std::fs::read_dir(&project_dir) {
            for entry in entries.flatten() {
                let dest = claude_state.join(entry.file_name());
                if !dest.exists() {
                    if entry.path().is_dir() {
                        copy_dir_recursive(&entry.path(), &dest);
                    } else {
                        std::fs::copy(entry.path(), &dest).ok();
                    }
                }
            }
        }
        std::fs::remove_dir_all(&project_dir).ok();
    }

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(claude_state, &project_dir).ok();
    }
}

/// Recursively copy a directory.
fn copy_dir_recursive(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).ok();
    let Ok(entries) = std::fs::read_dir(src) else {
        return;
    };
    for entry in entries.flatten() {
        let dest = dst.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir_recursive(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), &dest).ok();
        }
    }
}

/// Migrate old flat workspace layout to nested layout.
/// old: ~/tinyclaw-workspace/sultana/{files...}
/// new: ~/tinyclaw-workspace/sultana/default/{files...} + _meta/active
fn migrate_flat_workspace(agent_root: &Path, ws_name: &str) {
    let ws_dir = agent_root.join(ws_name);
    let tmp = agent_root.with_file_name(format!(
        ".{}_migrate_tmp",
        agent_root.file_name().unwrap_or_default().to_string_lossy()
    ));

    // Move everything to a temp dir, then back into the workspace subdir
    if std::fs::rename(agent_root, &tmp).is_err() {
        return;
    }
    std::fs::create_dir_all(&ws_dir).ok();

    if let Ok(entries) = std::fs::read_dir(&tmp) {
        for entry in entries.flatten() {
            let dest = ws_dir.join(entry.file_name());
            std::fs::rename(entry.path(), &dest).ok();
        }
    }
    std::fs::remove_dir_all(&tmp).ok();

    // Create _meta
    let meta = agent_root.join("_meta");
    std::fs::create_dir_all(meta.join("backups")).ok();
    std::fs::write(meta.join("active"), ws_name).ok();

    info!(
        "Migrated flat workspace to nested layout: {}",
        agent_root.display()
    );
}
