//! Manual end-to-end check against the REAL Claude Code (uses your
//! subscription; a few tiny prompts). Not run by default:
//!
//!   cargo test --test real_claude -- --ignored --nocapture
//!
//! Runs the cthread binary inside a PTY, drives it with keystrokes, and
//! writes every screen to $CTHREAD_E2E_OUT (default: target/e2e-screens.txt).

use cthread::pane::{Pane, SpawnSpec};
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

struct E2e {
    pane: Pane,
    log: std::fs::File,
}

impl E2e {
    fn screen(&self) -> String {
        self.pane.parser().screen().contents()
    }

    fn dump(&mut self, label: &str) {
        let s = self.screen();
        writeln!(self.log, "===== {label}\n{s}\n").unwrap();
    }

    fn wait(&mut self, label: &str, secs: u64, pred: impl Fn(&str) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let s = self.screen();
            // First run in a folder: accept Claude Code's trust prompt.
            if s.contains("Yes, I trust this folder") {
                // Default is "No, exit": move down to "Yes" first.
                std::thread::sleep(Duration::from_millis(1000));
                self.pane.write(b"\x1b[B");
                let t = Instant::now();
                while !self.screen().contains("❯ Yes") && t.elapsed() < Duration::from_secs(5) {
                    std::thread::sleep(Duration::from_millis(100));
                }
                self.dump("trust prompt: Yes selected");
                self.pane.write(b"\r");
                std::thread::sleep(Duration::from_millis(2000));
                continue;
            }
            if pred(&s) {
                self.dump(label);
                return;
            }
            if Instant::now() > deadline {
                self.dump(&format!("TIMEOUT: {label}"));
                panic!("timed out: {label}\n{s}");
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn keys(&self, b: &[u8]) {
        self.pane.write(b);
        std::thread::sleep(Duration::from_millis(150));
    }

    fn typ(&self, s: &str) {
        for c in s.chars() {
            let mut b = [0u8; 4];
            self.pane.write(c.encode_utf8(&mut b).as_bytes());
            std::thread::sleep(Duration::from_millis(15));
        }
    }
}

#[test]
#[ignore]
fn real_claude_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("e2e-proj");
    std::fs::create_dir(&project).unwrap();
    let out = std::env::var("CTHREAD_E2E_OUT")
        .unwrap_or_else(|_| format!("{}/target/e2e-screens.txt", env!("CARGO_MANIFEST_DIR")));
    let spec = SpawnSpec {
        program: env!("CARGO_BIN_EXE_cthread").into(),
        args: vec![],
        cwd: project.clone(),
        env: vec![
            ("CTHREAD_STATE_DIR".into(), tmp.path().join("state").to_string_lossy().into()),
            ("CTHREAD_LOG".into(), format!("{}/target/e2e.log", env!("CARGO_MANIFEST_DIR"))),
        ],
    };
    let pane = Pane::spawn(&spec, 45, 170, Arc::new(|_| {})).unwrap();
    let mut e = E2e { pane, log: std::fs::File::create(&out).unwrap() };

    e.wait("main started", 60, |s| s.contains("for shortcuts") || s.contains("Claude Code v"));
    std::thread::sleep(Duration::from_secs(2));
    e.typ("Reply with only the word apple.");
    e.keys(b"\r");
    e.wait("main answered", 120, |s| s.matches("apple").count() >= 2 && !s.contains("esc to interrupt"));
    std::thread::sleep(Duration::from_millis(1500));

    // New thread
    e.keys(&[0x1d]);
    e.keys(b"n");
    e.wait("new-thread modal", 10, |s| s.contains("Forks main as it is right now"));
    e.typ("fruit thread");
    e.keys(b"\r");
    e.wait("thread forked with history", 90, |s| s.contains("# fruit thread") && s.matches("apple").count() >= 3);
    std::thread::sleep(Duration::from_millis(1500));
    e.typ("Now reply with only the word banana.");
    e.keys(b"\r");
    e.wait("thread answered", 120, |s| s.contains("banana") && s.matches("banana").count() >= 2 && !s.contains("esc to interrupt"));
    std::thread::sleep(Duration::from_millis(1500));
    e.dump("after thread answer");

    // Pull into main
    e.keys(&[0x1d]);
    e.keys(b"p");
    e.wait("pull summary ready", 150, |s| s.contains("Enter insert into main"));
    e.keys(b"\r");
    e.wait("context in main input", 20, |s| s.contains("Context from thread") || s.contains("Pasted text"));

    // Quit
    e.keys(&[0x1d]);
    e.keys(b"q");
    std::thread::sleep(Duration::from_secs(5));
    e.dump("after quit");
    eprintln!("screens written to {out}");
}

