use std::path::Path;

use tracing::info;

use crate::config;

// Template files embedded at compile time
const AGENTS_MD: &str = include_str!("../templates/AGENTS.md");
const SOUL_MD: &str = include_str!("../templates/SOUL.md");


/// Ensure persona directory exists with soul template and empty subdirs.
/// Persona lives at tinyclaw_home/{persona_id}/.
pub fn ensure_persona(
    tinyclaw_home: &Path,
    persona_id: &str,
) {
    let persona = config::persona_dir(tinyclaw_home, persona_id);
    if persona.exists() {
        return;
    }

    std::fs::create_dir_all(&persona).ok();
    std::fs::create_dir_all(persona.join("skills")).ok();
    std::fs::create_dir_all(persona.join("claude")).ok();

    std::fs::write(persona.join("soul.md"), SOUL_MD).ok();

    info!("Initialized persona: {persona_id}");
}

/// Ensure workspace directory exists with infrastructure scaffolding.
/// Workspace lives at ~/{bot_id}-workspace/ (passed as workspace_dir).
/// Creates runtime dirs: logs/, files/, chats/.
/// Idempotent — safe to call multiple times.
pub fn ensure_workspace(
    workspace_dir: &Path,
    skills_source: Option<&Path>,
) {
    let fresh = !workspace_dir.exists();

    std::fs::create_dir_all(workspace_dir).ok();

    // Create runtime dirs (idempotent)
    for subdir in &["logs", "files", "chats"] {
        std::fs::create_dir_all(workspace_dir.join(subdir)).ok();
    }

    // Create .claude dir (CLAUDE.md is assembled at invocation time)
    let claude_dir = workspace_dir.join(".claude");
    std::fs::create_dir_all(&claude_dir).ok();

    // Write infrastructure files (only on fresh init)
    if fresh {
        std::fs::write(workspace_dir.join("AGENTS.md"), AGENTS_MD).ok();
    }

    // Symlink infrastructure skills (idempotent)
    if let Some(skills_src) = skills_source {
        if skills_src.exists() {
            let claude_skills = claude_dir.join("skills");
            if !claude_skills.exists() {
                #[cfg(unix)]
                std::os::unix::fs::symlink(skills_src, &claude_skills).ok();
            }
        }
    }

    if fresh {
        info!("Initialized workspace: {}", workspace_dir.display());
    }
}

/// Assemble .claude/CLAUDE.md by combining infrastructure template + persona soul.
/// Also sets up the Claude CLI session symlink for conversation continuity.
/// Called on every invocation (not just first run).
pub fn assemble_claude_md(
    workspace_dir: &Path,
    tinyclaw_home: &Path,
    persona_id: &str,
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

    // Write assembled CLAUDE.md
    let claude_dir = workspace_dir.join(".claude");
    std::fs::create_dir_all(&claude_dir).ok();
    std::fs::write(claude_dir.join("CLAUDE.md"), &assembled).ok();

    // Also write AGENTS.md for legacy compat
    std::fs::write(workspace_dir.join("AGENTS.md"), &assembled).ok();

    // Merge skills: infrastructure + persona
    setup_skills(&claude_dir, infra_skills, &persona.join("skills"));

    // Set up Claude CLI session symlink for conversation continuity
    setup_session_symlink(workspace_dir, &persona.join("claude"));
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

    // Symlink individual persona skills (bot-created)
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
