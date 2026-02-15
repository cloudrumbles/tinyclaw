use std::collections::HashMap;
use std::path::{Path, PathBuf};

use regex::Regex;
use tokio::process::Command;
use tracing::{info, error};

use crate::agent_setup::{ensure_agent_directory, update_agent_teammates};
use crate::errors::InvokeError;
use crate::types::{AgentConfig, TeamConfig, resolve_claude_model, resolve_codex_model};

/// Run a command and return its stdout. Strips Claude Code env vars from the
/// environment so spawned CLI processes don't think they're nested.
pub async fn run_command(
    command: &str,
    args: &[&str],
    cwd: Option<&Path>,
) -> Result<String, InvokeError> {
    // Build clean environment: strip CLAUDECODE and CLAUDE_CODE_* vars
    let env: HashMap<String, String> = std::env::vars()
        .filter(|(key, _)| key != "CLAUDECODE" && !key.starts_with("CLAUDE_CODE_"))
        .collect();

    let working_dir = cwd.unwrap_or_else(|| Path::new("."));

    let output = Command::new(command)
        .args(args)
        .current_dir(working_dir)
        .env_clear()
        .envs(&env)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                InvokeError::CommandNotFound(command.to_string())
            } else {
                InvokeError::Io(e)
            }
        })?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        // Strip control characters (keep newlines, tabs, printable chars)
        let re = Regex::new(r"[\x00-\x08\x0B\x0C\x0E-\x1F\x7F]").unwrap();
        Ok(re.replace_all(&stdout, "").to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let error_msg = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else if !stdout.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            format!(
                "Command exited with code {}",
                output.status.code().unwrap_or(-1)
            )
        };
        Err(InvokeError::CommandFailed(error_msg))
    }
}

/// Invoke a single agent with a message. Contains all Claude/Codex invocation logic.
pub async fn invoke_agent(
    agent: &AgentConfig,
    agent_id: &str,
    message: &str,
    workspace_path: &Path,
    should_reset: bool,
    agents: &HashMap<String, AgentConfig>,
    teams: &HashMap<String, TeamConfig>,
    skills_source: Option<&Path>,
) -> Result<String, InvokeError> {
    let agent_dir = workspace_path.join(agent_id);
    let is_new = !agent_dir.exists();

    // Ensure agent directory exists (blocking FS ops)
    let ad = agent_dir.clone();
    let ss = skills_source.map(PathBuf::from);
    let agents_clone = agents.clone();
    let teams_clone = teams.clone();
    let agent_id_owned = agent_id.to_string();

    tokio::task::spawn_blocking(move || {
        ensure_agent_directory(&ad, ss.as_deref());
        update_agent_teammates(&ad, &agent_id_owned, &agents_clone, &teams_clone);
    })
    .await
    .ok();

    if is_new {
        info!("Initialized agent directory: {}", agent_dir.display());
    }

    // Resolve working directory
    let working_dir = if !agent.working_directory.is_empty() {
        let wd = PathBuf::from(&agent.working_directory);
        if wd.is_absolute() {
            wd
        } else {
            workspace_path.join(&agent.working_directory)
        }
    } else {
        agent_dir
    };

    let provider = if agent.provider.is_empty() {
        "anthropic"
    } else {
        &agent.provider
    };

    if provider == "openai" {
        info!("Using Codex CLI (agent: {agent_id})");

        let should_resume = !should_reset;
        if should_reset {
            info!("Resetting Codex conversation for agent: {agent_id}");
        }

        let model_id = resolve_codex_model(&agent.model);
        let mut codex_args: Vec<String> = vec!["exec".into()];
        if should_resume {
            codex_args.extend(["resume".into(), "--last".into()]);
        }
        if !model_id.is_empty() {
            codex_args.extend(["--model".into(), model_id.to_string()]);
        }
        codex_args.extend([
            "--skip-git-repo-check".into(),
            "--dangerously-bypass-approvals-and-sandbox".into(),
            "--json".into(),
            message.to_string(),
        ]);

        let args_refs: Vec<&str> = codex_args.iter().map(|s| s.as_str()).collect();
        let codex_output = run_command("codex", &args_refs, Some(&working_dir)).await?;

        // Parse JSONL output and extract final agent_message
        let mut response = String::new();
        for line in codex_output.lines() {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
                if json.get("type").and_then(|t| t.as_str()) == Some("item.completed") {
                    if let Some(item) = json.get("item") {
                        if item.get("type").and_then(|t| t.as_str()) == Some("agent_message") {
                            if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                                response = text.to_string();
                            }
                        }
                    }
                }
            }
        }

        if response.is_empty() {
            Ok("Sorry, I could not generate a response from Codex.".into())
        } else {
            Ok(response)
        }
    } else {
        // Default to Claude (Anthropic)
        info!("Using Claude provider (agent: {agent_id})");

        let continue_conversation = !should_reset;
        if should_reset {
            info!("Resetting conversation for agent: {agent_id}");
        }

        let model_id = resolve_claude_model(&agent.model);
        let mut claude_args: Vec<String> = vec!["--dangerously-skip-permissions".into()];
        if !model_id.is_empty() {
            claude_args.extend(["--model".into(), model_id.to_string()]);
        }
        if continue_conversation {
            claude_args.push("-c".into());
        }
        claude_args.extend(["-p".into(), message.to_string()]);

        let args_refs: Vec<&str> = claude_args.iter().map(|s| s.as_str()).collect();
        run_command("claude", &args_refs, Some(&working_dir)).await.map_err(|e| {
            error!("Claude error (agent: {agent_id}): {e}");
            e
        })
    }
}
