//! Hook relay: Claude Code runs `cthread hook` for each event; it forwards the
//! hook JSON (plus the pane ID) over a Unix socket to the running TUI.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::Duration;

pub const EVENTS: &[&str] = &["SessionStart", "UserPromptSubmit", "Stop", "Notification", "SessionEnd"];

/// One line on the socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wire {
    pub pane: String,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HookEvent {
    SessionStart { session_id: String, source: String, transcript_path: Option<String>, cwd: Option<String> },
    UserPromptSubmit { session_id: String },
    Stop { session_id: String },
    /// `needs_you` is false for notifications that are not a permission or
    /// input wait (e.g. the 60-second idle reminder).
    Notification { message: String, needs_you: bool },
    SessionEnd { session_id: String, reason: String },
    Other(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct HookMsg {
    pub pane: String,
    pub event: HookEvent,
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str()).map(str::to_string)
}

pub fn parse_event(v: &Value) -> HookEvent {
    let name = s(v, "hook_event_name").unwrap_or_default();
    let sid = s(v, "session_id").unwrap_or_default();
    match name.as_str() {
        "SessionStart" => HookEvent::SessionStart {
            session_id: sid,
            source: s(v, "source").unwrap_or_else(|| "startup".into()),
            transcript_path: s(v, "transcript_path"),
            cwd: s(v, "cwd"),
        },
        "UserPromptSubmit" => HookEvent::UserPromptSubmit { session_id: sid },
        "Stop" => HookEvent::Stop { session_id: sid },
        "Notification" => {
            let message = s(v, "message").unwrap_or_default();
            let needs_you = match s(v, "notification_type").as_deref() {
                Some("permission_prompt") | Some("elicitation_dialog") => true,
                Some("idle_prompt") | Some("auth_success") => false,
                // Unknown or missing type: decide by the message text.
                _ => {
                    let m = message.to_lowercase();
                    !m.contains("waiting for your input") && (m.contains("permission") || m.contains("needs your") || m.contains("approve") || m.contains("input"))
                }
            };
            HookEvent::Notification { message, needs_you }
        }
        "SessionEnd" => HookEvent::SessionEnd { session_id: sid, reason: s(v, "reason").unwrap_or_default() },
        other => HookEvent::Other(other.to_string()),
    }
}

pub fn parse_line(line: &str) -> Option<HookMsg> {
    let w: Wire = serde_json::from_str(line).ok()?;
    Some(HookMsg { pane: w.pane, event: parse_event(&w.payload) })
}

/// Quote a path for /bin/sh.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The extra settings file passed with `--settings`; adds cthread's hooks on
/// top of the user's own (Spike S2: settings hooks merge).
pub fn settings_json(exe: &Path) -> String {
    let cmd = format!("{} hook", sh_quote(&exe.to_string_lossy()));
    let mut hooks = serde_json::Map::new();
    for ev in EVENTS {
        hooks.insert(
            ev.to_string(),
            serde_json::json!([{ "hooks": [{ "type": "command", "command": cmd, "timeout": 5 }] }]),
        );
    }
    serde_json::to_string_pretty(&serde_json::json!({ "hooks": hooks })).unwrap()
}

/// `cthread hook`: read hook JSON from stdin, forward it, always exit 0.
pub fn run_hook_client() {
    let mut input = String::new();
    let _ = std::io::stdin().take(4 * 1024 * 1024).read_to_string(&mut input);
    let (Ok(sock), Ok(pane)) = (std::env::var("CTHREAD_SOCK"), std::env::var("CTHREAD_PANE")) else {
        return;
    };
    let Ok(payload) = serde_json::from_str::<Value>(&input) else { return };
    let Ok(mut stream) = UnixStream::connect(&sock) else { return };
    let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
    let mut line = serde_json::to_string(&Wire { pane, payload }).unwrap_or_default();
    line.push('\n');
    let _ = stream.write_all(line.as_bytes());
}

/// Bind the socket and forward parsed messages to `on_msg` from a background
/// thread. Removes a stale socket file first (the instance lock guarantees no
/// other cthread owns it).
pub fn listen<F>(path: &Path, on_msg: F) -> std::io::Result<()>
where
    F: Fn(HookMsg) + Send + Sync + 'static,
{
    let _ = std::fs::remove_file(path);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let listener = UnixListener::bind(path)?;
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    let on_msg = std::sync::Arc::new(on_msg);
    std::thread::Builder::new().name("hook-listener".into()).spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { continue };
            let on_msg = on_msg.clone();
            std::thread::spawn(move || {
                let _ = conn.set_read_timeout(Some(Duration::from_secs(5)));
                for line in BufReader::new(conn).lines() {
                    let Ok(line) = line else { break };
                    if let Some(m) = parse_line(&line) {
                        on_msg(m);
                    }
                }
            });
        }
    })?;
    Ok(())
}

/// Whether a cthread instance is listening on `path` (for `cthread doctor`).
pub fn probe(path: &Path) -> bool {
    UnixStream::connect(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let p = format!("{}/tests/fixtures/hooks/{name}.json", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    #[test]
    fn parse_fixtures() {
        match parse_event(&fixture("session_start_startup")) {
            HookEvent::SessionStart { session_id, source, transcript_path, .. } => {
                assert_eq!(session_id, "e85d1141-4528-4332-8d2c-ab32b1fba3de");
                assert_eq!(source, "startup");
                assert!(transcript_path.unwrap().ends_with(".jsonl"));
            }
            e => panic!("{e:?}"),
        }
        assert!(matches!(
            parse_event(&fixture("session_start_fork")),
            HookEvent::SessionStart { ref source, .. } if source == "fork"
        ));
        assert!(matches!(parse_event(&fixture("user_prompt_submit")), HookEvent::UserPromptSubmit { .. }));
        assert!(matches!(parse_event(&fixture("stop")), HookEvent::Stop { .. }));
        assert!(matches!(parse_event(&fixture("session_end")), HookEvent::SessionEnd { ref reason, .. } if reason == "other"));
        assert!(matches!(
            parse_event(&fixture("notification_permission")),
            HookEvent::Notification { needs_you: true, .. }
        ));
        assert!(matches!(
            parse_event(&fixture("notification_idle")),
            HookEvent::Notification { needs_you: false, .. }
        ));
    }

    #[test]
    fn wire_roundtrip() {
        let line = serde_json::to_string(&Wire { pane: "p1".into(), payload: fixture("stop") }).unwrap();
        let m = parse_line(&line).unwrap();
        assert_eq!(m.pane, "p1");
        assert!(matches!(m.event, HookEvent::Stop { .. }));
        assert!(parse_line("not json").is_none());
    }

    #[test]
    fn settings_have_all_events() {
        let j: Value = serde_json::from_str(&settings_json(Path::new("/opt/my app/cthread"))).unwrap();
        for ev in EVENTS {
            let cmd = j["hooks"][ev][0]["hooks"][0]["command"].as_str().unwrap();
            assert_eq!(cmd, "'/opt/my app/cthread' hook");
        }
        assert_eq!(sh_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn socket_relay() {
        let d = tempfile::tempdir().unwrap();
        let sock = d.path().join("s.sock");
        let (tx, rx) = std::sync::mpsc::channel();
        listen(&sock, move |m| {
            let _ = tx.send(m);
        })
        .unwrap();
        assert!(probe(&sock));
        let mut c = UnixStream::connect(&sock).unwrap();
        let line = serde_json::to_string(&Wire { pane: "pp".into(), payload: fixture("user_prompt_submit") }).unwrap();
        writeln!(c, "{line}").unwrap();
        drop(c);
        let m = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(m.pane, "pp");
    }
}
