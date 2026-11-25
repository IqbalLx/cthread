//! Pull summaries (FR-40): `claude -p --resume <thread> --fork-session` in a
//! throwaway, non-persisted fork. No PTY; stdout is captured.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct SummaryRequest {
    pub job: u64,
    /// Program plus leading arguments (`claude.pull_command`).
    pub command: Vec<String>,
    pub session_id: String,
    pub cwd: PathBuf,
    pub prompt: String,
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SummaryResult {
    pub job: u64,
    pub result: Result<String, String>,
}

/// Arguments after the program: the command's own leading arguments, then
/// the flags for a throwaway, non-persisted fork.
pub fn args(req: &SummaryRequest) -> Vec<String> {
    let mut a: Vec<String> = req.command.iter().skip(1).cloned().collect();
    a.extend([
        "-p".into(),
        "--resume".into(),
        req.session_id.clone(),
        "--fork-session".into(),
        "--no-session-persistence".into(),
        req.prompt.clone(),
    ]);
    a
}

/// Start a summary job in the background. Returns a cancel flag.
pub fn start(req: SummaryRequest, done: Arc<dyn Fn(SummaryResult) + Send + Sync>) -> Arc<AtomicBool> {
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    std::thread::spawn(move || {
        let r = run(&req, &c);
        if !c.load(Ordering::Relaxed) {
            done(SummaryResult { job: req.job, result: r });
        }
    });
    cancel
}

fn run(req: &SummaryRequest, cancel: &AtomicBool) -> Result<String, String> {
    let program = req.command.first().ok_or("no claude command configured")?;
    let mut cmd = Command::new(program);
    for k in crate::pane::SESSION_MARKERS {
        cmd.env_remove(k);
    }
    let mut child = cmd
        .args(args(req))
        .current_dir(&req.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("CTHREAD_PANE")
        .env_remove("CTHREAD_SOCK")
        .spawn()
        .map_err(|e| format!("could not start {program}: {e}"))?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out_t = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let err_t = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let deadline = Instant::now() + req.timeout;
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("cancelled".into());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("timed out after {} s", req.timeout.as_secs()));
        }
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let out = out_t.join().unwrap_or_default();
    let err = err_t.join().unwrap_or_default();
    if !status.success() {
        let msg = err.lines().filter(|l| !l.starts_with("Warning:")).collect::<Vec<_>>().join(" ");
        return Err(if msg.trim().is_empty() { format!("claude exited with {status}") } else { msg });
    }
    let text = out.trim().to_string();
    if text.is_empty() { Err("empty summary".into()) } else { Ok(text) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn script(dir: &std::path::Path, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("fakeclaude");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p.to_string_lossy().to_string()
    }

    fn req(claude: String, timeout: u64) -> SummaryRequest {
        SummaryRequest {
            job: 7,
            command: vec![claude],
            session_id: "sid".into(),
            cwd: std::env::temp_dir(),
            prompt: "sum".into(),
            timeout: Duration::from_millis(timeout),
        }
    }

    #[test]
    fn ok_timeout_cancel() {
        let d = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let done: Arc<dyn Fn(SummaryResult) + Send + Sync> = Arc::new(move |r| {
            let _ = tx.lock().unwrap().send(r);
        });

        let ok = script(d.path(), "echo \"- args: $*\"");
        start(req(ok.clone(), 5000), done.clone());
        let r = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(r.job, 7);
        assert_eq!(r.result.unwrap(), "- args: -p --resume sid --fork-session --no-session-persistence sum");

        // a custom command keeps its own leading arguments first
        let mut custom = req(ok, 5000);
        custom.command.extend(["code".to_string(), "--profile".to_string(), "my work".to_string()]);
        start(custom, done.clone());
        let r = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(r.result.unwrap(), "- args: code --profile my work -p --resume sid --fork-session --no-session-persistence sum");

        let slow = d.path().join("slow");
        std::fs::create_dir(&slow).unwrap();
        let slow = script(&slow, "sleep 10");
        start(req(slow.clone(), 200), done.clone());
        let r = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(r.result.unwrap_err().contains("timed out"));

        let c = start(req(slow, 5000), done.clone());
        c.store(true, Ordering::Relaxed);
        assert!(rx.recv_timeout(Duration::from_millis(800)).is_err());
    }
}
