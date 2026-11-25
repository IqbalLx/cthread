//! A PTY-backed session pane: spawns a child, parses its output into a vt100
//! screen, forwards input, resizes, and stops it gracefully.

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub const SCROLLBACK: usize = 5000;

/// Environment variables that mark a process as running *inside* another
/// Claude Code session. cthread's sessions are top-level sessions, so these
/// must not leak into them (if inherited, Claude Code turns off transcript
/// saving and forking breaks). Configuration variables such as
/// `CLAUDE_CONFIG_DIR` are kept.
pub const SESSION_MARKERS: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_CHILD_NOTES_DIR",
    "CLAUDE_PID",
];

/// Answers the terminal queries that vt100 leaves unhandled, so TUI programs
/// that probe the terminal (cursor position, device attributes) don't stall.
#[derive(Default)]
pub struct Responder {
    pub out: Vec<u8>,
    pub bell: bool,
}

impl vt100::Callbacks for Responder {
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.bell = true;
    }
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        _i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        let p0 = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        match (i1, c) {
            // DSR: cursor position report / status
            (None, 'n') if p0 == 6 => {
                let (r, col) = screen.cursor_position();
                self.out.extend_from_slice(format!("\x1b[{};{}R", r + 1, col + 1).as_bytes());
            }
            (None, 'n') if p0 == 5 => self.out.extend_from_slice(b"\x1b[0n"),
            (Some(b'?'), 'n') if p0 == 6 => {
                let (r, col) = screen.cursor_position();
                self.out.extend_from_slice(format!("\x1b[?{};{}R", r + 1, col + 1).as_bytes());
            }
            // DA1 / DA2
            (None, 'c') if p0 == 0 => self.out.extend_from_slice(b"\x1b[?62;22c"),
            (Some(b'>'), 'c') if p0 == 0 => self.out.extend_from_slice(b"\x1b[>1;10;0c"),
            _ => {}
        }
    }
}

pub type Parser = vt100::Parser<Responder>;

#[derive(Debug, Clone)]
pub struct SpawnSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
}

/// Notifications from a pane's background threads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneNotice {
    Output(String),
    Exited(String),
}

pub struct Pane {
    pub id: String,
    parser: Arc<Mutex<Parser>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Option<Box<dyn MasterPty + Send>>,
    pid: Option<u32>,
    size: (u16, u16),
    got_output: Arc<AtomicBool>,
    exited: Arc<AtomicBool>,
    pub spawned_at: Instant,
}

fn nz(v: u16) -> NonZeroU16 {
    NonZeroU16::new(v.max(1)).unwrap()
}

impl Pane {
    pub fn spawn(
        spec: &SpawnSpec,
        rows: u16,
        cols: u16,
        notify: Arc<dyn Fn(PaneNotice) + Send + Sync>,
    ) -> Result<Pane> {
        let id = uuid::Uuid::new_v4().to_string();
        let (rows, cols) = (rows.max(2), cols.max(10));
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .context("openpty")?;
        let mut cmd = CommandBuilder::new(&spec.program);
        cmd.args(&spec.args);
        cmd.cwd(&spec.cwd);
        for k in SESSION_MARKERS {
            cmd.env_remove(k);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("CTHREAD_PANE", &id);
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        let mut child = pair
            .slave
            .spawn_command(cmd)
            .with_context(|| format!("failed to start {}", spec.program))?;
        drop(pair.slave);
        let pid = child.process_id();
        let mut reader = pair.master.try_clone_reader()?;
        let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(pair.master.take_writer()?));
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            nz(rows),
            nz(cols),
            SCROLLBACK,
            Responder::default(),
        )));
        let got_output = Arc::new(AtomicBool::new(false));
        let exited = Arc::new(AtomicBool::new(false));

        // Reader: bytes → parser; answer queries; notify UI.
        {
            let parser = parser.clone();
            let writer = writer.clone();
            let got_output = got_output.clone();
            let notify = notify.clone();
            let id = id.clone();
            std::thread::Builder::new().name(format!("pty-read-{id}")).spawn(move || {
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            crate::dlog!("pane {id} read {n}: {:?}", String::from_utf8_lossy(&buf[..n.min(200)]));
                            let reply = {
                                let mut p = parser.lock().unwrap();
                                p.process(&buf[..n]);
                                std::mem::take(&mut p.callbacks_mut().out)
                            };
                            if !reply.is_empty() {
                                crate::dlog!("pane {id} reply {:?}", String::from_utf8_lossy(&reply));
                                let mut w = writer.lock().unwrap();
                                let _ = w.write_all(&reply);
                                let _ = w.flush();
                            }
                            got_output.store(true, Ordering::Relaxed);
                            notify(PaneNotice::Output(id.clone()));
                        }
                    }
                }
            })?;
        }
        // Waiter: reaps the child and reports exit.
        {
            let exited = exited.clone();
            let id = id.clone();
            std::thread::Builder::new().name(format!("pty-wait-{id}")).spawn(move || {
                let _ = child.wait();
                exited.store(true, Ordering::Relaxed);
                notify(PaneNotice::Exited(id));
            })?;
        }

        Ok(Pane {
            id,
            parser,
            writer,
            master: Some(pair.master),
            pid,
            size: (rows, cols),
            got_output,
            exited,
            spawned_at: Instant::now(),
        })
    }

    pub fn parser(&self) -> MutexGuard<'_, Parser> {
        self.parser.lock().unwrap()
    }

    pub fn got_output(&self) -> bool {
        self.got_output.load(Ordering::Relaxed)
    }

    pub fn has_exited(&self) -> bool {
        self.exited.load(Ordering::Relaxed)
    }

    pub fn size(&self) -> (u16, u16) {
        self.size
    }

    pub fn write(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let mut w = self.writer.lock().unwrap();
        let _ = w.write_all(bytes);
        let _ = w.flush();
    }

    /// Send input typed by the user: also jumps back to the live screen.
    pub fn input(&self, bytes: &[u8]) {
        {
            let mut p = self.parser();
            if p.screen().scrollback() > 0 {
                p.screen_mut().set_scrollback(0);
            }
        }
        self.write(bytes);
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(2), cols.max(10));
        if (rows, cols) == self.size {
            return;
        }
        self.size = (rows, cols);
        // Resize the parser first: the child redraws as soon as it gets
        // SIGWINCH, and that output must be parsed at the new size.
        self.parser().screen_mut().set_size(nz(rows), nz(cols));
        if let Some(m) = &self.master {
            let _ = m.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        }
    }

    /// Scroll cthread's own scrollback by `delta` lines (positive = up).
    pub fn scroll(&self, delta: i32) {
        let mut p = self.parser();
        let cur = p.screen().scrollback() as i32;
        let next = (cur + delta).max(0) as usize;
        p.screen_mut().set_scrollback(next);
    }

    pub fn app_cursor(&self) -> bool {
        self.parser().screen().application_cursor()
    }

    pub fn take_bell(&self) -> bool {
        std::mem::take(&mut self.parser().callbacks_mut().bell)
    }

    /// Graceful stop (FR-33): SIGTERM to the child's process group, then
    /// SIGKILL after `grace`. Runs in the background; returns immediately.
    pub fn stop(mut self, grace: Duration) {
        let master = self.master.take();
        let exited = self.exited.clone();
        let pid = self.pid;
        std::thread::spawn(move || {
            if let Some(pid) = pid {
                signal(pid, libc::SIGTERM);
                let deadline = Instant::now() + grace;
                while Instant::now() < deadline && !exited.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(50));
                }
                if !exited.load(Ordering::Relaxed) {
                    signal(pid, libc::SIGKILL);
                }
            }
            drop(master);
        });
    }

    /// Stop and wait until the child has exited (used on quit).
    pub fn stop_blocking(mut self, grace: Duration) {
        let exited = self.exited.clone();
        if let Some(pid) = self.pid {
            signal(pid, libc::SIGTERM);
            let deadline = Instant::now() + grace;
            while Instant::now() < deadline && !exited.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(20));
            }
            if !exited.load(Ordering::Relaxed) {
                signal(pid, libc::SIGKILL);
            }
        }
        drop(self.master.take());
    }
}

fn signal(pid: u32, sig: i32) {
    unsafe {
        // The child is a session leader (setsid), so its pid is its pgid.
        if libc::kill(-(pid as i32), sig) != 0 {
            libc::kill(pid as i32, sig);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn spec(script: &str) -> SpawnSpec {
        SpawnSpec { program: "/bin/sh".into(), args: vec!["-c".into(), script.into()], cwd: "/".into(), env: vec![] }
    }

    fn wait_for(p: &Pane, needle: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if p.parser().screen().contents().contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn spawn_echo_exit() {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let p = Pane::spawn(
            &spec("printf 'hello %s' \"$CTHREAD_PANE\"; read x; echo got:$x"),
            10,
            40,
            Arc::new(move |n| {
                let _ = tx.lock().unwrap().send(n);
            }),
        )
        .unwrap();
        assert!(wait_for(&p, "hello "));
        assert!(p.parser().screen().contents().contains(&p.id[..8]));
        p.input(b"abc\r");
        assert!(wait_for(&p, "got:abc"));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(PaneNotice::Exited(id)) = rx.recv_timeout(Duration::from_millis(100)) {
                assert_eq!(id, p.id);
                break;
            }
            assert!(Instant::now() < deadline, "no exit notice");
        }
        assert!(p.has_exited());
    }

    #[test]
    fn answers_cursor_query_and_resizes() {
        let mut p = Pane::spawn(
            &spec("stty raw -echo; printf 'ab\\033[6n'; r=$(dd bs=1 count=6 2>/dev/null); printf '[%s]' \"${r#?}\"; stty size; sleep 5"),
            10,
            40,
            Arc::new(|_| {}),
        )
        .unwrap();
        assert!(wait_for(&p, "[[1;3R]"), "{}", p.parser().screen().contents());
        p.resize(12, 50);
        assert_eq!(p.size(), (12, 50));
        p.stop_blocking(Duration::from_secs(1));
    }

    #[test]
    fn strips_claude_session_markers() {
        unsafe { std::env::set_var("CLAUDE_CODE_CHILD_SESSION", "1") };
        let p = Pane::spawn(&spec("echo \"m=[${CLAUDE_CODE_CHILD_SESSION}]\"; sleep 2"), 5, 40, Arc::new(|_| {})).unwrap();
        assert!(wait_for(&p, "m=[]"), "{}", p.parser().screen().contents());
        p.stop_blocking(Duration::from_millis(200));
    }

    #[test]
    fn stop_kills_stubborn_child() {
        let p = Pane::spawn(&spec("trap '' TERM; echo ready; sleep 30"), 5, 20, Arc::new(|_| {})).unwrap();
        assert!(wait_for(&p, "ready"));
        let exited = p.exited.clone();
        let t = Instant::now();
        p.stop_blocking(Duration::from_millis(300));
        let deadline = Instant::now() + Duration::from_secs(3);
        while !exited.load(Ordering::Relaxed) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(exited.load(Ordering::Relaxed));
        assert!(t.elapsed() < Duration::from_secs(3));
    }
}

#[cfg(test)]
mod responder_tests {
    use super::*;

    #[test]
    fn replies_to_queries() {
        let mut p: Parser = vt100::Parser::new_with_callbacks(NonZeroU16::new(10).unwrap(), NonZeroU16::new(40).unwrap(), 0, Responder::default());
        p.process(b"ab\x1b[6n");
        assert_eq!(std::mem::take(&mut p.callbacks_mut().out), b"\x1b[1;3R");
        p.process(b"\x1b[?6n");
        assert_eq!(std::mem::take(&mut p.callbacks_mut().out), b"\x1b[?1;3R");
        p.process(b"\x1b[c");
        assert_eq!(std::mem::take(&mut p.callbacks_mut().out), b"\x1b[?62;22c");
    }
}
