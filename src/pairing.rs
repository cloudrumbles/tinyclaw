use rand::RngExt;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

use crate::errors::PairingError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingPendingEntry {
    pub channel: String,
    #[serde(rename = "senderId")]
    pub sender_id: String,
    pub sender: String,
    pub code: String,
    #[serde(rename = "createdAt")]
    pub created_at: u64,
    #[serde(rename = "lastSeenAt")]
    pub last_seen_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingApprovedEntry {
    pub channel: String,
    #[serde(rename = "senderId")]
    pub sender_id: String,
    pub sender: String,
    #[serde(rename = "approvedAt")]
    pub approved_at: u64,
    #[serde(rename = "approvedCode")]
    pub approved_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PairingState {
    pub pending: Vec<PairingPendingEntry>,
    pub approved: Vec<PairingApprovedEntry>,
}

pub struct PairingCheckResult {
    pub approved: bool,
    pub code: Option<String>,
    pub is_new_pending: bool,
}

fn sender_key(channel: &str, sender_id: &str) -> String {
    format!("{channel}::{sender_id}")
}

const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

fn random_pairing_code() -> String {
    let mut rng = rand::rng();
    (0..8)
        .map(|_| {
            let idx = rng.random_range(0..ALPHABET.len());
            ALPHABET[idx] as char
        })
        .collect()
}

fn create_unique_code(state: &PairingState) -> String {
    let existing: HashSet<String> = state
        .pending
        .iter()
        .map(|e| e.code.to_uppercase())
        .chain(
            state
                .approved
                .iter()
                .filter_map(|e| e.approved_code.as_ref())
                .map(|c| c.to_uppercase()),
        )
        .collect();

    for _ in 0..20 {
        let candidate = random_pairing_code();
        if !existing.contains(&candidate) {
            return candidate;
        }
    }

    // Fallback: timestamp-based
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("{:X}", ts)
        .chars()
        .rev()
        .take(8)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

pub fn load_pairing_state(pairing_file: &Path) -> PairingState {
    if !pairing_file.exists() {
        return PairingState::default();
    }
    let Ok(data) = std::fs::read_to_string(pairing_file) else {
        return PairingState::default();
    };
    serde_json::from_str(&data).unwrap_or_default()
}

pub fn save_pairing_state(
    pairing_file: &Path,
    state: &PairingState,
) -> Result<(), PairingError> {
    if let Some(parent) = pairing_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = pairing_file.with_extension("json.tmp");
    let data = serde_json::to_string_pretty(state)?;
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, pairing_file)?;
    Ok(())
}

pub fn ensure_sender_paired(
    pairing_file: &Path,
    channel: &str,
    sender_id: &str,
    sender: &str,
) -> PairingCheckResult {
    let mut state = load_pairing_state(pairing_file);
    let key = sender_key(channel, sender_id);

    // Check approved
    if let Some(entry) = state
        .approved
        .iter_mut()
        .find(|e| sender_key(&e.channel, &e.sender_id) == key)
    {
        if entry.sender != sender {
            entry.sender = sender.to_string();
            let _ = save_pairing_state(pairing_file, &state);
        }
        return PairingCheckResult {
            approved: true,
            code: None,
            is_new_pending: false,
        };
    }

    // Check existing pending
    if let Some(entry) = state
        .pending
        .iter_mut()
        .find(|e| e.channel == channel && e.sender_id == sender_id)
    {
        let now = now_millis();
        entry.last_seen_at = now;
        entry.sender = sender.to_string();
        let code = entry.code.clone();
        let _ = save_pairing_state(pairing_file, &state);
        return PairingCheckResult {
            approved: false,
            code: Some(code),
            is_new_pending: false,
        };
    }

    // Create new pending entry
    let code = create_unique_code(&state);
    let now = now_millis();
    state.pending.push(PairingPendingEntry {
        channel: channel.to_string(),
        sender_id: sender_id.to_string(),
        sender: sender.to_string(),
        code: code.clone(),
        created_at: now,
        last_seen_at: now,
    });
    let _ = save_pairing_state(pairing_file, &state);

    PairingCheckResult {
        approved: false,
        code: Some(code),
        is_new_pending: true,
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
