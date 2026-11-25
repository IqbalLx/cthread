//! Where cthread keeps things, and the Claude Code path details it depends on.
//!
//! Everything that depends on Claude Code internals (the transcript folder
//! layout) is isolated in this file so it can be fixed in one place.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Per-folder file locations.
#[derive(Debug, Clone)]
pub struct ProjectPaths {
    pub project_dir: PathBuf,
    pub state_dir: PathBuf,
    pub key: String,
    pub state_file: PathBuf,
    pub lock_file: PathBuf,
    pub socket: PathBuf,
    pub settings_file: PathBuf,
}

/// `<XDG state dir>/cthread` (Linux), `~/Library/Application Support/cthread`
/// (macOS). `CTHREAD_STATE_DIR` overrides it (used by tests).
pub fn state_dir() -> PathBuf {
    if let Ok(d) = std::env::var("CTHREAD_STATE_DIR")
        && !d.is_empty() {
            return PathBuf::from(d);
        }
    let base = directories::BaseDirs::new();
    match base {
        Some(b) => match b.state_dir() {
            Some(s) => s.join("cthread"),
            None => b.data_dir().join("cthread"),
        },
        None => std::env::temp_dir().join("cthread"),
    }
}

/// First 16 hex chars of sha256 of the absolute folder path.
pub fn project_key(project_dir: &Path) -> String {
    let mut h = Sha256::new();
    h.update(project_dir.to_string_lossy().as_bytes());
    let d = h.finalize();
    d.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

impl ProjectPaths {
    pub fn new(project_dir: &Path) -> ProjectPaths {
        Self::with_state_dir(project_dir, state_dir())
    }

    pub fn with_state_dir(project_dir: &Path, state_dir: PathBuf) -> ProjectPaths {
        let project_dir = canonical(project_dir);
        let key = project_key(&project_dir);
        let mut socket = state_dir.join(format!("{key}.sock"));
        // sun_path is limited to ~104 bytes on macOS.
        if socket.as_os_str().len() > 100 {
            let uid = unsafe { libc::getuid() };
            socket = PathBuf::from(format!("/tmp/cthread-{uid}-{key}.sock"));
        }
        ProjectPaths {
            state_file: state_dir.join(format!("{key}.json")),
            lock_file: state_dir.join(format!("{key}.lock")),
            settings_file: state_dir.join(format!("{key}.settings.json")),
            socket,
            key,
            state_dir,
            project_dir,
        }
    }
}

pub fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// `~/.claude` (or `$CLAUDE_CONFIG_DIR`).
pub fn claude_config_dir() -> PathBuf {
    if let Ok(d) = std::env::var("CLAUDE_CONFIG_DIR")
        && !d.is_empty() {
            return PathBuf::from(d);
        }
    directories::BaseDirs::new()
        .map(|b| b.home_dir().join(".claude"))
        .unwrap_or_else(|| PathBuf::from(".claude"))
}

/// Claude Code's folder-name encoding for `~/.claude/projects/<encoded cwd>`:
/// every character that is not an ASCII letter or digit becomes `-`.
/// Verified in Spike S3 (docs/spikes.md). Claude Code internal detail.
pub fn encode_project_dir(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Transcript folder for sessions started in `cwd`.
pub fn transcript_dir(cwd: &Path) -> PathBuf {
    claude_config_dir().join("projects").join(encode_project_dir(&canonical(cwd)))
}

/// Expected transcript path of `session_id` started in `cwd`.
pub fn transcript_path(cwd: &Path, session_id: &str) -> PathBuf {
    transcript_dir(cwd).join(format!("{session_id}.jsonl"))
}

/// Find a transcript anywhere under `~/.claude/projects` (fallback when the
/// encoding guess misses, e.g. for very long paths).
pub fn find_transcript(session_id: &str) -> Option<PathBuf> {
    let root = claude_config_dir().join("projects");
    let name = format!("{session_id}.jsonl");
    for e in std::fs::read_dir(root).ok()?.flatten() {
        let p = e.path().join(&name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Whether a transcript holds at least one user message (FR-11).
pub fn transcript_has_messages(path: &Path) -> bool {
    let Ok(s) = std::fs::read_to_string(path) else { return false };
    s.lines().any(|l| {
        l.contains("\"type\":\"user\"") || l.contains("\"type\": \"user\"")
    })
}

/// Search PATH for an executable (or accept an existing path).
pub fn which(cmd: &str) -> Option<PathBuf> {
    let p = Path::new(cmd);
    if cmd.contains('/') {
        return is_executable(p).then(|| p.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(cmd)).find(|c| is_executable(c))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_matches_claude_code() {
        // Observed in Spike S3.
        assert_eq!(
            encode_project_dir(Path::new("/private/tmp/claude-501/-Users-x/scratchpad/spike/proj-wt.x_y")),
            "-private-tmp-claude-501--Users-x-scratchpad-spike-proj-wt-x-y"
        );
        assert_eq!(encode_project_dir(Path::new("/Users/me/code/shop")), "-Users-me-code-shop");
        assert_eq!(encode_project_dir(Path::new("/a b/c.d")), "-a-b-c-d");
    }

    #[test]
    fn key_is_16_hex() {
        let k = project_key(Path::new("/Users/me/code/shop"));
        assert_eq!(k.len(), 16);
        assert!(k.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(k, project_key(Path::new("/Users/me/code/shop2")));
    }

    #[test]
    fn transcript_messages() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("t.jsonl");
        std::fs::write(&p, "{\"type\":\"summary\"}\n").unwrap();
        assert!(!transcript_has_messages(&p));
        std::fs::write(&p, "{\"type\":\"summary\"}\n{\"type\":\"user\",\"message\":{}}\n").unwrap();
        assert!(transcript_has_messages(&p));
        assert!(!transcript_has_messages(&d.path().join("missing")));
    }
}
