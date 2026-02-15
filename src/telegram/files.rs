use std::path::{Path, PathBuf};

/// Split a long message for Telegram's 4096 char limit.
pub fn split_message(text: &str, max_length: usize) -> Vec<String> {
    if text.len() <= max_length {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= max_length {
            chunks.push(remaining.to_string());
            break;
        }

        // Try to split at a newline boundary
        let mut split_index = remaining[..max_length]
            .rfind('\n')
            .unwrap_or(0);

        // Fall back to space boundary
        if split_index == 0 {
            split_index = remaining[..max_length]
                .rfind(' ')
                .unwrap_or(0);
        }

        // Hard-cut if no good boundary
        if split_index == 0 {
            split_index = max_length;
        }

        chunks.push(remaining[..split_index].to_string());
        remaining = &remaining[split_index..];
        // Strip leading newline
        if remaining.starts_with('\n') {
            remaining = &remaining[1..];
        }
    }

    chunks
}

/// Get file extension from MIME type.
pub fn ext_from_mime(mime: &str) -> &str {
    match mime {
        "image/jpeg" => ".jpg",
        "image/png" => ".png",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "audio/ogg" => ".ogg",
        "audio/mpeg" => ".mp3",
        "video/mp4" => ".mp4",
        "application/pdf" => ".pdf",
        _ => "",
    }
}

/// Sanitize a filename for safe filesystem use.
pub fn sanitize_filename(name: &str) -> String {
    let base = Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file.bin");
    let sanitized: String = base
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = sanitized.trim();
    if trimmed.is_empty() {
        "file.bin".into()
    } else {
        trimmed.into()
    }
}

/// Ensure a file has an extension, adding a fallback if missing.
pub fn ensure_file_extension(name: &str, fallback_ext: &str) -> String {
    if Path::new(name).extension().is_some() {
        name.to_string()
    } else {
        format!("{name}{fallback_ext}")
    }
}

/// Build a unique file path, appending _1, _2, etc. if the file already exists.
pub fn build_unique_file_path(dir: &Path, preferred_name: &str) -> PathBuf {
    let clean = sanitize_filename(preferred_name);
    let ext = Path::new(&clean)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    let stem = Path::new(&clean)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file");

    let mut candidate = dir.join(&clean);
    let mut counter = 1;
    while candidate.exists() {
        candidate = dir.join(format!("{stem}_{counter}{ext}"));
        counter += 1;
    }
    candidate
}
