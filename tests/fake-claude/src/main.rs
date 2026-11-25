//! A stand-in for the `claude` binary, for tests. Accepts the flags cthread
//! uses, keeps transcripts under $CLAUDE_CONFIG_DIR/projects, fires hooks from
//! the --settings file, and echoes prompts.
//!
//! Prompts with special meaning:
//!   /clear          new session (SessionEnd + SessionStart source=clear)
//!   /resume <id>    switch to another session (SessionStart source=resume)
//!   perm            fire a permission Notification and wait for a key
//!   slow <ms>       sleep before answering (stay Working)
//!   crash           exit with status 3
//!
//! Env: FAKE_CLAUDE_PULL_DELAY_MS delays `-p` output.

use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Args {
    session_id: Option<String>,
    resume: Option<String>,
    fork: bool,
    settings: Option<PathBuf>,
    print: bool,
    prompt: Option<String>,
}

fn parse_args() -> Args {
    let mut a = Args { session_id: None, resume: None, fork: false, settings: None, print: false, prompt: None };
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        match x.as_str() {
            "--version" => {
                println!("0.0.0 (fake claude)");
                std::process::exit(0);
            }
            "--session-id" => a.session_id = it.next(),
            "--resume" | "-r" => a.resume = it.next(),
            "--fork-session" => a.fork = true,
            "--settings" => a.settings = it.next().map(PathBuf::from),
            "-p" | "--print" => a.print = true,
            "--no-session-persistence" => {}
            "--model" => {
                it.next();
            }
            s if s.starts_with('-') => {}
            s => a.prompt = Some(s.to_string()),
        }
    }
    a
}

fn config_dir() -> PathBuf {
    std::env::var("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".claude")
    })
}

fn encode(p: &Path) -> String {
    p.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

fn cwd() -> PathBuf {
    std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap()
}

fn transcript(sid: &str) -> PathBuf {
    config_dir().join("projects").join(encode(&cwd())).join(format!("{sid}.jsonl"))
}

fn find(sid: &str) -> Option<PathBuf> {
    let own = transcript(sid);
    if own.exists() {
        return Some(own);
    }
    for e in std::fs::read_dir(config_dir().join("projects")).ok()?.flatten() {
        let p = e.path().join(format!("{sid}.jsonl"));
        if p.exists() {
            return Some(p);
        }
    }
    None
}

fn load(path: &Path) -> Vec<(String, String)> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| Some((v["type"].as_str()?.to_string(), v["message"]["content"].as_str()?.to_string())))
        .collect()
}

fn append(sid: &str, kind: &str, text: &str) {
    let p = transcript(sid);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(p).unwrap();
    let line = json!({"type": kind, "sessionId": sid, "message": {"role": kind, "content": text}});
    writeln!(f, "{line}").unwrap();
}

struct Hooks {
    settings: Option<PathBuf>,
}

impl Hooks {
    fn fire(&self, event: &str, sid: &str, extra: Value) {
        let Some(path) = &self.settings else { return };
        let Ok(s) = std::fs::read_to_string(path) else { return };
        let Ok(v) = serde_json::from_str::<Value>(&s) else { return };
        let mut payload = json!({
            "session_id": sid,
            "transcript_path": transcript(sid).to_string_lossy(),
            "cwd": cwd().to_string_lossy(),
            "hook_event_name": event,
        });
        if let (Some(p), Some(e)) = (payload.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                p.insert(k.clone(), v.clone());
            }
        }
        for group in v["hooks"][event].as_array().into_iter().flatten() {
            for h in group["hooks"].as_array().into_iter().flatten() {
                let Some(cmd) = h["command"].as_str() else { continue };
                if let Ok(mut c) = Command::new("/bin/sh").arg("-c").arg(cmd).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
                    let _ = c.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
                    let _ = c.wait();
                }
            }
        }
    }
}

fn raw_mode() {
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut t) == 0 {
            libc::cfmakeraw(&mut t);
            libc::tcsetattr(0, libc::TCSANOW, &t);
        }
    }
}

struct Ui {
    sid: String,
    history: Vec<(String, String)>,
    input: String,
    note: String,
}

impl Ui {
    fn draw(&self) {
        let mut o = String::from("\x1b[H\x1b[2J");
        o.push_str(&format!("fake-claude {}\r\n", &self.sid[..8.min(self.sid.len())]));
        for (k, t) in &self.history {
            let mark = if k == "user" { ">" } else { "⏺" };
            o.push_str(&format!("{mark} {t}\r\n"));
        }
        if !self.note.is_empty() {
            o.push_str(&format!("{}\r\n", self.note));
        }
        o.push_str(&format!("> {}", self.input));
        let mut out = std::io::stdout();
        let _ = out.write_all(o.as_bytes());
        let _ = out.flush();
    }
}

fn main() {
    let a = parse_args();
    let (sid, source, history) = match (&a.resume, a.fork) {
        (Some(src), true) => {
            let sid = a.session_id.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let hist = find(src).map(|p| load(&p)).unwrap_or_default();
            if !a.print {
                for (k, t) in &hist {
                    append(&sid, k, t);
                }
            }
            (sid, "fork", hist)
        }
        (Some(src), false) => {
            let Some(path) = find(src) else {
                // Same as the real binary.
                println!("No conversation found with session ID: {src}");
                std::process::exit(1);
            };
            let hist = load(&path);
            (src.clone(), "resume", hist)
        }
        (None, _) => (a.session_id.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string()), "startup", vec![]),
    };

    if a.print {
        if let Ok(ms) = std::env::var("FAKE_CLAUDE_PULL_DELAY_MS") {
            std::thread::sleep(std::time::Duration::from_millis(ms.parse().unwrap_or(0)));
        }
        let last = history.iter().rev().find(|(k, _)| k == "user").map(|(_, t)| t.clone()).unwrap_or_default();
        println!("- {} messages; last: {last}", history.len());
        return;
    }

    let hooks = Hooks { settings: a.settings.clone() };
    raw_mode();
    print!("\x1b[?2004h");
    let mut ui = Ui { sid: sid.clone(), history, input: String::new(), note: String::new() };
    hooks.fire("SessionStart", &ui.sid, json!({"source": source}));
    ui.draw();

    let mut stdin = std::io::stdin();
    let mut buf = [0u8; 4096];
    let mut pending: Vec<u8> = vec![];
    let mut ctrl_c = 0;
    loop {
        let n = match stdin.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        pending.extend_from_slice(&buf[..n]);
        let mut i = 0;
        while i < pending.len() {
            let rest = &pending[i..];
            if rest.starts_with(b"\x1b[200~") || rest.starts_with(b"\x1b[201~") {
                i += 6;
                continue;
            }
            if rest[0] == 0x1b {
                // skip other escape sequences: ESC [ ... final
                let mut j = 1;
                if rest.len() > 1 && rest[1] == b'[' {
                    j = 2;
                    while j < rest.len() && !(0x40..=0x7e).contains(&rest[j]) {
                        j += 1;
                    }
                    j += 1;
                } else if rest.len() > 1 {
                    j = 2;
                }
                i += j.min(rest.len());
                continue;
            }
            let b = rest[0];
            i += 1;
            match b {
                0x03 => {
                    ctrl_c += 1;
                    if ctrl_c >= 2 {
                        return;
                    }
                }
                b'\r' => {
                    let line = std::mem::take(&mut ui.input);
                    submit(&mut ui, &hooks, line.trim());
                }
                0x7f => {
                    ui.input.pop();
                }
                b'\n' => ui.input.push('\n'),
                b if b >= 0x20 => {
                    // collect a full UTF-8 char
                    let len = match b {
                        0xf0..=0xff => 4,
                        0xe0..=0xef => 3,
                        0xc0..=0xdf => 2,
                        _ => 1,
                    };
                    let start = i - 1;
                    let end = (start + len).min(pending.len());
                    ui.input.push_str(&String::from_utf8_lossy(&pending[start..end]));
                    i = end;
                }
                _ => {}
            }
        }
        pending.clear();
        ui.draw();
    }
}

fn submit(ui: &mut Ui, hooks: &Hooks, line: &str) {
    if line.is_empty() {
        return;
    }
    ui.note.clear();
    if line == "/clear" {
        hooks.fire("SessionEnd", &ui.sid, json!({"reason": "clear"}));
        ui.sid = uuid::Uuid::new_v4().to_string();
        ui.history.clear();
        hooks.fire("SessionStart", &ui.sid, json!({"source": "clear"}));
        return;
    }
    if let Some(id) = line.strip_prefix("/resume ") {
        ui.sid = id.trim().to_string();
        ui.history = find(&ui.sid).map(|p| load(&p)).unwrap_or_default();
        hooks.fire("SessionStart", &ui.sid, json!({"source": "resume"}));
        return;
    }
    if line == "crash" {
        std::process::exit(3);
    }
    hooks.fire("UserPromptSubmit", &ui.sid, json!({"prompt": line}));
    append(&ui.sid, "user", line);
    ui.history.push(("user".into(), line.into()));
    ui.draw();
    if line == "perm" {
        ui.note = "Allow fake tool? (y/n)".into();
        ui.draw();
        hooks.fire(
            "Notification",
            &ui.sid,
            json!({"message": "Claude needs your permission to use Bash", "notification_type": "permission_prompt"}),
        );
        let mut b = [0u8; 1];
        let _ = std::io::stdin().read(&mut b);
        ui.note.clear();
    }
    if let Some(ms) = line.strip_prefix("slow ") {
        std::thread::sleep(std::time::Duration::from_millis(ms.trim().parse().unwrap_or(500)));
    }
    let reply = format!("echo: {line}");
    append(&ui.sid, "assistant", &reply);
    ui.history.push(("assistant".into(), reply));
    hooks.fire("Stop", &ui.sid, json!({"stop_hook_active": false}));
}
