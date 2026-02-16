use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use regex::Regex;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{info, warn, error};

use crate::agent_setup::{ensure_persona, ensure_workspace, assemble_claude_md};
use crate::config;
use crate::errors::InvokeError;
use crate::types::{BotConfig, resolve_claude_model};

/// Default idle timeout: kill the process if no output for this long.
/// Claude CLI goes silent during extended thinking and tool use, so this
/// needs to be generous. 180s catches truly stuck processes without
/// killing active work.
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(180);

/// Absolute wall-clock cap so a truly runaway process can't run forever.
const MAX_WALL_CLOCK: Duration = Duration::from_secs(30 * 60);

/// Run a command and return its stdout. Strips Claude Code env vars from the
/// environment so spawned CLI processes don't think they're nested.
///
/// Uses an **idle timeout**: the process is killed only if it produces no output
/// (stdout or stderr) for `idle_timeout`. This lets long-running but actively
/// streaming CLI sessions complete, while still catching stuck processes.
pub async fn run_command(
    command: &str,
    args: &[&str],
    cwd: Option<&Path>,
    idle_timeout: Duration,
) -> Result<String, InvokeError> {
    // Build clean environment: strip CLAUDECODE and CLAUDE_CODE_* vars that cause
    // nested CLI detection, but keep CLAUDE_CODE_OAUTH_TOKEN for authentication.
    let env: HashMap<String, String> = std::env::vars()
        .filter(|(key, _)| {
            if key == "CLAUDECODE" {
                return false;
            }
            if key.starts_with("CLAUDE_CODE_") && key != "CLAUDE_CODE_OAUTH_TOKEN" {
                return false;
            }
            true
        })
        .collect();

    let working_dir = cwd.unwrap_or_else(|| Path::new("."));

    let mut child = Command::new(command)
        .args(args)
        .current_dir(working_dir)
        .env_clear()
        .envs(&env)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                InvokeError::CommandNotFound(command.to_string())
            } else {
                InvokeError::Io(e)
            }
        })?;

    let mut stdout_pipe = child.stdout.take().expect("stdout piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr piped");

    let mut stdout_buf = Vec::new();
    let mut stderr_buf = Vec::new();
    let mut stdout_chunk = [0u8; 4096];
    let mut stderr_chunk = [0u8; 4096];
    let mut stdout_done = false;
    let mut stderr_done = false;
    let started = Instant::now();

    // Read stdout/stderr incrementally. Each loop iteration sets a fresh idle
    // deadline — if neither pipe produces data before it fires, we kill the child.
    loop {
        // Safety: absolute wall-clock cap
        if started.elapsed() > MAX_WALL_CLOCK {
            warn!(
                "Wall-clock cap ({}s) hit: {} {:?}",
                MAX_WALL_CLOCK.as_secs(), command, args
            );
            child.kill().await.ok();
            return Err(InvokeError::Timeout(
                MAX_WALL_CLOCK.as_secs(),
                format!("{} {}", command, args.join(" ")),
            ));
        }

        tokio::select! {
            result = stdout_pipe.read(&mut stdout_chunk), if !stdout_done => {
                match result {
                    Ok(0) => stdout_done = true,
                    Ok(n) => stdout_buf.extend_from_slice(&stdout_chunk[..n]),
                    Err(e) => return Err(InvokeError::Io(e)),
                }
            }
            result = stderr_pipe.read(&mut stderr_chunk), if !stderr_done => {
                match result {
                    Ok(0) => stderr_done = true,
                    Ok(n) => stderr_buf.extend_from_slice(&stderr_chunk[..n]),
                    Err(e) => return Err(InvokeError::Io(e)),
                }
            }
            _ = tokio::time::sleep(idle_timeout) => {
                warn!(
                    "Idle timeout ({}s, no output): {} {:?}",
                    idle_timeout.as_secs(), command, args
                );
                child.kill().await.ok();
                return Err(InvokeError::Timeout(
                    idle_timeout.as_secs(),
                    format!("{} {}", command, args.join(" ")),
                ));
            }
        }

        if stdout_done && stderr_done {
            break;
        }
    }

    let status = child.wait().await.map_err(InvokeError::Io)?;

    if status.success() {
        let stdout = String::from_utf8_lossy(&stdout_buf).to_string();
        // Strip control characters (keep newlines, tabs, printable chars)
        let re = Regex::new(r"[\x00-\x08\x0B\x0C\x0E-\x1F\x7F]").unwrap();
        Ok(re.replace_all(&stdout, "").to_string())
    } else {
        let stderr = String::from_utf8_lossy(&stderr_buf).to_string();
        let stdout = String::from_utf8_lossy(&stdout_buf).to_string();
        let error_msg = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else if !stdout.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            format!(
                "Command exited with code {}",
                status.code().unwrap_or(-1)
            )
        };
        Err(InvokeError::CommandFailed(error_msg))
    }
}

/// Build a human-readable status from a tool name and its (possibly partial) input JSON.
/// If `input` parses and contains a useful field, include it in the status.
fn tool_status(name: &str, input: &str) -> String {
    let detail = serde_json::from_str::<serde_json::Value>(input)
        .ok()
        .and_then(|v| {
            let s = match name {
                "Bash" => v.get("command").and_then(|c| c.as_str()).map(|cmd| {
                    // Replace long absolute paths with just the filename
                    let re = regex::Regex::new(r"/[\w./-]{20,}/([\w._-]+)").unwrap();
                    re.replace_all(cmd, "$1").to_string()
                }),
                "Read" => v.get("file_path").and_then(|p| p.as_str()).map(|s| {
                    s.rsplit('/').next().unwrap_or(s).to_string()
                }),
                "Edit" | "Write" => v.get("file_path").and_then(|p| p.as_str()).map(|s| {
                    s.rsplit('/').next().unwrap_or(s).to_string()
                }),
                "Grep" => v.get("pattern").and_then(|p| p.as_str()).map(|s| s.to_string()),
                "Glob" => v.get("pattern").and_then(|p| p.as_str()).map(|s| s.to_string()),
                "WebSearch" => v.get("query").and_then(|q| q.as_str()).map(|s| s.to_string()),
                "WebFetch" => v.get("url").and_then(|u| u.as_str()).map(|s| s.to_string()),
                _ => None,
            };
            // Truncate long details
            s.map(|d| if d.len() > 100 { format!("{}...", &d[..97]) } else { d })
        });

    let base = match name {
        "Read" | "NotebookRead" => "reading",
        "Glob" | "Grep" => "searching",
        "Edit" | "Write" | "NotebookEdit" => "editing",
        "Bash" => "running",
        "WebSearch" => "searching the web",
        "WebFetch" => "fetching",
        "Task" => "working with a subagent",
        "TaskOutput" => "waiting on subagent",
        "TodoWrite" => "updating tasks",
        "EnterPlanMode" | "ExitPlanMode" => "planning",
        other => other,
    };

    match detail {
        Some(d) => format!("{base}: {d}"),
        None => format!("{base}..."),
    }
}

/// Run a Claude CLI command with `--output-format stream-json --verbose`, parse
/// JSONL events to detect tool usage (sent via `status_tx`), and return the
/// final text result.
///
/// Same idle timeout logic as `run_command`, but additionally:
/// - Parses stdout as newline-delimited JSON
/// - Sends tool_use status updates through `status_tx`
/// - Extracts the final result text from the `{"type":"result"}` event
async fn run_claude_streaming(
    command: &str,
    args: &[&str],
    cwd: Option<&Path>,
    idle_timeout: Duration,
    status_tx: mpsc::Sender<String>,
) -> Result<String, InvokeError> {
    let env: HashMap<String, String> = std::env::vars()
        .filter(|(key, _)| {
            if key == "CLAUDECODE" {
                return false;
            }
            if key.starts_with("CLAUDE_CODE_") && key != "CLAUDE_CODE_OAUTH_TOKEN" {
                return false;
            }
            true
        })
        .collect();

    let working_dir = cwd.unwrap_or_else(|| Path::new("."));

    let mut child = Command::new(command)
        .args(args)
        .current_dir(working_dir)
        .env_clear()
        .envs(&env)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                InvokeError::CommandNotFound(command.to_string())
            } else {
                InvokeError::Io(e)
            }
        })?;

    let mut stdout_pipe = child.stdout.take().expect("stdout piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr piped");

    let mut stderr_buf = Vec::new();
    let mut stdout_chunk = [0u8; 4096];
    let mut stderr_chunk = [0u8; 4096];
    let mut stdout_done = false;
    let mut stderr_done = false;
    let started = Instant::now();

    // Line buffer for incomplete JSONL lines from stdout
    let mut line_buf = String::new();
    // Final result text extracted from the stream
    let mut result_text = String::new();
    // Track current tool_use for accumulating input_json_delta
    let mut current_tool_name: Option<String> = None;
    let mut current_tool_input = String::new();
    let mut last_status_sent = String::new();

    loop {
        if started.elapsed() > MAX_WALL_CLOCK {
            warn!(
                "Wall-clock cap ({}s) hit: {} {:?}",
                MAX_WALL_CLOCK.as_secs(), command, args
            );
            child.kill().await.ok();
            return Err(InvokeError::Timeout(
                MAX_WALL_CLOCK.as_secs(),
                format!("{} {}", command, args.join(" ")),
            ));
        }

        tokio::select! {
            result = stdout_pipe.read(&mut stdout_chunk), if !stdout_done => {
                match result {
                    Ok(0) => stdout_done = true,
                    Ok(n) => {
                        let chunk_str = String::from_utf8_lossy(&stdout_chunk[..n]);
                        line_buf.push_str(&chunk_str);

                        // Process complete lines
                        while let Some(newline_pos) = line_buf.find('\n') {
                            let line: String = line_buf.drain(..=newline_pos).collect();
                            let line = line.trim();
                            if line.is_empty() {
                                continue;
                            }

                            if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
                                let event_type = json.get("type").and_then(|t| t.as_str());

                                if event_type == Some("stream_event") {
                                    if let Some(event) = json.get("event") {
                                        let inner_type = event.get("type").and_then(|t| t.as_str());

                                        // content_block_start: new tool_use begins
                                        if inner_type == Some("content_block_start") {
                                            if let Some(block) = event.get("content_block") {
                                                if block.get("type").and_then(|t| t.as_str())
                                                    == Some("tool_use")
                                                {
                                                    if let Some(name) =
                                                        block.get("name").and_then(|n| n.as_str())
                                                    {
                                                        current_tool_name = Some(name.to_string());
                                                        current_tool_input.clear();
                                                        let status = tool_status(name, "");
                                                        if status != last_status_sent {
                                                            last_status_sent = status.clone();
                                                            status_tx.send(status).await.ok();
                                                        }
                                                    }
                                                }
                                            }
                                        }

                                        // content_block_delta: accumulate input_json_delta
                                        if inner_type == Some("content_block_delta") {
                                            if let Some(delta) = event.get("delta") {
                                                if delta.get("type").and_then(|t| t.as_str())
                                                    == Some("input_json_delta")
                                                {
                                                    if let Some(partial) =
                                                        delta.get("partial_json").and_then(|p| p.as_str())
                                                    {
                                                        current_tool_input.push_str(partial);
                                                        // Try to extract details from accumulated input
                                                        if let Some(ref name) = current_tool_name {
                                                            let status = tool_status(name, &current_tool_input);
                                                            if status != last_status_sent {
                                                                last_status_sent = status.clone();
                                                                status_tx.send(status).await.ok();
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }

                                        // content_block_stop: clear tracking state
                                        if inner_type == Some("content_block_stop") {
                                            current_tool_name = None;
                                            current_tool_input.clear();
                                        }
                                    }
                                }

                                // Complete assistant turn: tool_use blocks have full input
                                if event_type == Some("assistant") {
                                    if let Some(content) = json
                                        .get("message")
                                        .and_then(|m| m.get("content"))
                                        .and_then(|c| c.as_array())
                                    {
                                        for block in content {
                                            if block.get("type").and_then(|t| t.as_str())
                                                == Some("tool_use")
                                            {
                                                if let Some(name) =
                                                    block.get("name").and_then(|n| n.as_str())
                                                {
                                                    let input_str = block.get("input")
                                                        .map(|v| v.to_string())
                                                        .unwrap_or_default();
                                                    let status = tool_status(name, &input_str);
                                                    if status != last_status_sent {
                                                        last_status_sent = status.clone();
                                                        status_tx.send(status).await.ok();
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }

                                // Capture the final result text
                                if event_type == Some("result") {
                                    if let Some(text) =
                                        json.get("result").and_then(|r| r.as_str())
                                    {
                                        result_text = text.to_string();
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => return Err(InvokeError::Io(e)),
                }
            }
            result = stderr_pipe.read(&mut stderr_chunk), if !stderr_done => {
                match result {
                    Ok(0) => stderr_done = true,
                    Ok(n) => stderr_buf.extend_from_slice(&stderr_chunk[..n]),
                    Err(e) => return Err(InvokeError::Io(e)),
                }
            }
            _ = tokio::time::sleep(idle_timeout) => {
                warn!(
                    "Idle timeout ({}s, no output): {} {:?}",
                    idle_timeout.as_secs(), command, args
                );
                child.kill().await.ok();
                return Err(InvokeError::Timeout(
                    idle_timeout.as_secs(),
                    format!("{} {}", command, args.join(" ")),
                ));
            }
        }

        if stdout_done && stderr_done {
            break;
        }
    }

    let status = child.wait().await.map_err(InvokeError::Io)?;

    if status.success() {
        // Strip control characters from result
        let re = Regex::new(r"[\x00-\x08\x0B\x0C\x0E-\x1F\x7F]").unwrap();
        Ok(re.replace_all(&result_text, "").to_string())
    } else {
        let stderr = String::from_utf8_lossy(&stderr_buf).to_string();
        let error_msg = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else if !result_text.trim().is_empty() {
            result_text.trim().to_string()
        } else {
            format!(
                "Command exited with code {}",
                status.code().unwrap_or(-1)
            )
        };
        Err(InvokeError::CommandFailed(error_msg))
    }
}

/// Invoke the bot with a message. Contains all Claude/Codex invocation logic.
pub async fn invoke_bot(
    bot: &BotConfig,
    message: &str,
    tinyclaw_home: &Path,
    should_reset: bool,
    skills_source: Option<&Path>,
    status_tx: Option<mpsc::Sender<String>>,
) -> Result<String, InvokeError> {
    let t_invoke = std::time::Instant::now();

    // Resolve persona and workspace
    let persona_id = bot.persona.as_deref().unwrap_or(&bot.bot_id).to_string();
    let workspace_dir = config::bot_workspace(&bot.bot_id);

    // Set up persona, workspace, and assembled CLAUDE.md (blocking FS ops)
    {
        let home = tinyclaw_home.to_path_buf();
        let ws_dir = workspace_dir.clone();
        let pid = persona_id.clone();
        let ss = skills_source.map(PathBuf::from);

        tokio::task::spawn_blocking(move || {
            ensure_persona(&home, &pid);
            ensure_workspace(&ws_dir, ss.as_deref());
            assemble_claude_md(&ws_dir, &home, &pid, ss.as_deref());
        })
        .await
        .ok();
    }
    info!("[timing] bot_setup: {:?}", t_invoke.elapsed());

    // Working directory is the active workspace
    let working_dir = workspace_dir;

    let idle_timeout = bot
        .timeout
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_IDLE_TIMEOUT);

    let bot_id = &bot.bot_id;

    info!("Using Claude provider (bot: {bot_id})");

    let continue_conversation = !should_reset;
    if should_reset {
        info!("Resetting conversation for bot: {bot_id}");
    }

    let model_id = resolve_claude_model(&bot.model);
    let mut claude_args: Vec<String> = vec!["--dangerously-skip-permissions".into()];
    if !model_id.is_empty() {
        claude_args.extend(["--model".into(), model_id.to_string()]);
    }
    if continue_conversation {
        claude_args.push("-c".into());
    }

    // When streaming, use stream-json output to parse tool events
    let use_streaming = status_tx.is_some();
    if use_streaming {
        claude_args.extend([
            "--output-format".into(),
            "stream-json".into(),
            "--verbose".into(),
            "--include-partial-messages".into(),
        ]);
    }

    claude_args.extend(["-p".into(), message.to_string()]);

    let args_refs: Vec<&str> = claude_args.iter().map(|s| s.as_str()).collect();
    info!("[timing] pre-claude: {:?}", t_invoke.elapsed());

    let result = if let Some(tx) = status_tx {
        run_claude_streaming("claude", &args_refs, Some(&working_dir), idle_timeout, tx).await
    } else {
        run_command("claude", &args_refs, Some(&working_dir), idle_timeout).await
    };

    info!("[timing] claude_cli: {:?}", t_invoke.elapsed());
    result.map_err(|e| {
        error!("Claude error (bot: {bot_id}): {e}");
        e
    })
}
