//! Headless end-to-end tests: cthread's App driven with ratatui's TestBackend
//! against the fake claude binary (tests/fake-claude). No Claude subscription
//! needed.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use cthread::app::{App, AppEvent, AppOptions, Focus, Modal, PullPhase};
use cthread::config::{CommandLine, Config};
use cthread::paths::{self, ProjectPaths};
use cthread::policy::{Activity, Status};
use cthread::state::{self, Loaded, State, Store};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(15);

struct Env {
    fake_claude: PathBuf,
    _claude_cfg: tempfile::TempDir,
}

/// Build fake-claude once and point CLAUDE_CONFIG_DIR at a temp folder.
fn env() -> &'static Env {
    static ENV: OnceLock<Env> = OnceLock::new();
    ENV.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let status = Command::new(env!("CARGO"))
            .args(["build", "-q", "-p", "fake-claude"])
            .current_dir(root)
            .status()
            .expect("cargo build fake-claude");
        assert!(status.success());
        let target = std::env::var("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|_| root.join("target"));
        let claude_cfg = tempfile::tempdir().unwrap();
        // SAFETY: set once, before any test spawns threads that read them.
        unsafe {
            std::env::set_var("CLAUDE_CONFIG_DIR", claude_cfg.path());
            std::env::set_var("TZ", "UTC");
        }
        Env { fake_claude: target.join("debug/fake-claude"), _claude_cfg: claude_cfg }
    })
}

struct H {
    app: App,
    rx: Receiver<AppEvent>,
    term: Terminal<TestBackend>,
    project: PathBuf,
    state_dir: PathBuf,
    _tmp: Option<tempfile::TempDir>,
}

fn cfg() -> Config {
    let mut c = Config::default();
    c.claude.command = Some(CommandLine::Argv(vec![env().fake_claude.to_string_lossy().to_string()]));
    c.pull.timeout_seconds = 10;
    c
}

impl H {
    fn new(w: u16, h: u16, c: Config) -> H {
        env();
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("shop");
        std::fs::create_dir(&project).unwrap();
        let state_dir = tmp.path().join("state");
        let mut h = H::open(&project, &state_dir, w, h, c);
        h._tmp = Some(tmp);
        h
    }

    fn open(project: &Path, state_dir: &Path, w: u16, h: u16, c: Config) -> H {
        let p = ProjectPaths::with_state_dir(project, state_dir.to_path_buf());
        let store = match state::load(&p.state_file) {
            Loaded::Ok(s) => Store::new(p.state_file.clone(), s, false),
            _ => Store::new(p.state_file.clone(), State::new(&p.project_dir), false),
        };
        let (tx, rx) = channel();
        let tx = Mutex::new(tx);
        let notify: cthread::app::Notify = Arc::new(move |e| {
            let _ = tx.lock().unwrap().send(e);
        });
        let mut app = App::new(c, p, store, AppOptions { hook_exe: env!("CARGO_BIN_EXE_cthread").into() }, notify).unwrap();
        app.start((w, h)).unwrap();
        let term = Terminal::new(TestBackend::new(w, h)).unwrap();
        H { app, rx, term, project: paths::canonical(project), state_dir: state_dir.to_path_buf(), _tmp: None }
    }

    fn draw(&mut self) {
        let app = &mut self.app;
        self.term.draw(|f| cthread::ui::draw(f, app)).unwrap();
        self.app.sync_sizes();
    }

    fn screen(&self) -> String {
        let buf = self.term.backend().buffer();
        let mut s = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                s.push_str(buf[(x, y)].symbol());
            }
            s.push('\n');
        }
        s
    }

    fn step(&mut self) {
        if let Ok(e) = self.rx.recv_timeout(Duration::from_millis(15)) {
            self.app.handle(e);
            while let Ok(e) = self.rx.try_recv() {
                self.app.handle(e);
            }
        }
        self.app.tick(Instant::now());
        self.draw();
    }

    fn wait(&mut self, what: &str, mut pred: impl FnMut(&H) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.step();
            if pred(self) {
                return;
            }
            if Instant::now() > deadline {
                panic!("timed out waiting for {what}\n{}", self.screen());
            }
        }
    }

    fn wait_text(&mut self, needle: &str) {
        let n = needle.to_string();
        self.wait(&format!("text {needle:?}"), move |h| h.screen().contains(&n));
    }

    fn settle(&mut self, ms: u64) {
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end {
            self.step();
        }
    }

    fn key(&mut self, code: KeyCode, m: KeyModifiers) {
        self.app.handle(AppEvent::Term(Event::Key(KeyEvent::new(code, m))));
        self.step();
    }

    fn ch(&mut self, c: char) {
        let m = if c.is_ascii_uppercase() { KeyModifiers::SHIFT } else { KeyModifiers::NONE };
        self.key(KeyCode::Char(c), m);
    }

    fn typ(&mut self, s: &str) {
        for c in s.chars() {
            self.ch(c);
        }
    }

    fn enter(&mut self) {
        self.key(KeyCode::Enter, KeyModifiers::NONE);
    }

    fn cmd(&mut self, c: char) {
        self.key(KeyCode::Char(']'), KeyModifiers::CONTROL);
        self.ch(c);
    }

    /// Send a prompt in the focused pane and wait for the echo.
    fn say(&mut self, s: &str) {
        self.typ(s);
        self.enter();
        self.wait_text(&format!("echo: {s}"));
    }

    fn main_id(&self) -> String {
        self.app.store.state.active_main.clone().unwrap()
    }

    fn new_thread(&mut self, title: &str) -> String {
        self.cmd('n');
        assert!(matches!(self.app.modal, Some(Modal::NewThread { .. })), "{}", self.screen());
        self.typ(title);
        self.enter();
        assert!(self.app.modal.is_none(), "{}", self.screen());
        let id = self.app.open_thread().unwrap().to_string();
        self.wait_text(&format!("# {title}"));
        id
    }

    fn thread_status(&self, id: &str) -> Status {
        let t = self.app.store.state.thread(id).unwrap();
        self.app.thread_status(t)
    }

    fn quit(mut self) {
        self.app.shutdown();
    }
}

fn wait_ready(h: &mut H) {
    h.wait_text("fake-claude");
    // SessionStart must have reached us before we rely on IDs.
    h.wait("main SessionStart", |h| !h.app.main.first_start);
}

#[test]
fn create_thread_forks_and_stays_isolated() {
    let mut h = H::new(160, 40, cfg());
    wait_ready(&mut h);

    // FR-11: main needs history first.
    h.cmd('n');
    assert!(h.app.modal.is_none());
    assert!(h.screen().contains("Send a message in main first"));

    h.say("hello main");
    // FR-10: title is required.
    h.cmd('n');
    h.enter();
    assert!(matches!(h.app.modal, Some(Modal::NewThread { .. })));
    assert!(h.screen().contains("Title is required"));
    h.typ("stripe vs paddle pricing");
    h.enter();
    assert!(h.app.modal.is_none());
    assert_eq!(h.app.focus, Focus::Thread);
    let tid = h.app.open_thread().unwrap().to_string();
    h.wait_text("# stripe vs paddle pricing");
    h.wait_text("forked from main");

    // The thread starts with main's history…
    h.wait("thread shows main history", |h| h.screen().matches("> hello main").count() >= 2);
    // …and its own messages never reach main.
    h.say("thread only question");
    let main_t = paths::transcript_path(&h.project, &h.main_id());
    let main_text = std::fs::read_to_string(&main_t).unwrap();
    assert!(main_text.contains("hello main"));
    assert!(!main_text.contains("thread only"));

    // SessionStart (source=fork) confirmed the pre-assigned ID.
    h.wait("thread id confirmed", |h| h.app.store.state.thread(&tid).unwrap().session_id.is_some());
    let t = h.app.store.state.thread(&tid).unwrap();
    assert!(t.pending_session_id.is_none());
    assert_eq!(t.turns, 1);

    // FR-12: no nesting from inside a thread.
    h.cmd('n');
    assert!(h.app.modal.is_none());
    assert!(h.screen().contains("Threads can't have threads"));
    assert_eq!(h.app.visible_threads().len(), 1);

    // Sidebar row present.
    assert!(h.screen().contains("stripe vs paddle"));
    h.quit();
}

#[test]
fn status_working_unread_needs_you() {
    let mut h = H::new(160, 40, cfg());
    wait_ready(&mut h);
    h.say("hi");
    let tid = h.new_thread("long task");
    h.wait("thread ready", |h| h.app.store.state.thread(&tid).unwrap().session_id.is_some());
    h.typ("slow 1500");
    h.enter();
    h.wait("working", |h| h.thread_status(&tid) == Status::Working);
    // switch away: focus main
    h.cmd('h');
    assert_eq!(h.app.focus, Focus::Main);
    h.wait("unread", |h| h.thread_status(&tid) == Status::Unread);
    assert!(h.screen().contains("● long task"));
    // opening clears it
    h.cmd('l');
    assert_eq!(h.app.focus, Focus::Thread);
    assert_eq!(h.thread_status(&tid), Status::Idle);

    // Needs you
    h.typ("perm");
    h.enter();
    h.wait("needs you", |h| h.thread_status(&tid) == Status::NeedsYou);
    assert!(h.screen().contains("! long task"));
    h.ch('y');
    h.wait("idle again", |h| h.thread_status(&tid) == Status::Idle);

    // Main has the same glyphs (FR-22).
    h.cmd('m');
    h.typ("slow 800");
    h.enter();
    h.wait("main working", |h| h.app.main.activity == Activity::Working);
    h.cmd('l');
    h.wait("main unread", |h| h.app.main_unread);
    h.quit();
}

#[test]
fn discard_and_restore() {
    let mut c = cfg();
    c.threads.max_live = 1;
    let mut h = H::new(160, 40, c);
    wait_ready(&mut h);
    h.say("base");
    let a = h.new_thread("alpha");
    h.say("alpha says");
    h.cmd('m');
    let b = h.new_thread("beta");
    h.wait("beta started", |h| h.app.store.state.thread(&b).unwrap().session_id.is_some());
    // alpha is not open and over the limit: discarded.
    h.wait("alpha discarded", |h| h.thread_status(&a) == Status::Discarded);
    assert!(h.screen().contains("◌ alpha"));
    // reopen alpha: restored with its conversation (FR-32)
    h.cmd('k');
    assert_eq!(h.app.open_thread(), Some(a.as_str()));
    h.wait_text("echo: alpha says");
    // beta is now the non-open one and gets discarded
    h.wait("beta discarded", |h| h.thread_status(&b) == Status::Discarded);
    h.quit();
}

#[test]
fn pull_inserts_without_sending() {
    let mut c = cfg();
    let mut h = H::new(160, 40, {
        c.pull.prompt = "sum it".into();
        c
    });
    wait_ready(&mut h);
    h.say("main topic");
    let _t = h.new_thread("research");
    // FR-42: nothing new yet
    h.cmd('p');
    assert!(h.app.modal.is_none());
    assert!(h.screen().contains("Nothing new in this thread"));
    h.say("paddle is merchant of record");
    h.cmd('p');
    assert!(matches!(h.app.modal, Some(Modal::Pull { .. })));
    h.wait("summary", |h| matches!(h.app.modal, Some(Modal::Pull { phase: PullPhase::Ready, .. })));
    assert!(h.screen().contains("last: paddle is merchant of record"));
    // edit in-app, then insert
    h.ch('e');
    h.typ(" (edited)");
    h.key(KeyCode::Esc, KeyModifiers::NONE);
    h.enter();
    assert!(h.app.modal.is_none());
    assert_eq!(h.app.focus, Focus::Main);
    h.wait_text("[Context from thread \"research\"]");
    h.wait_text("(edited)");
    // Never pressed Enter: no echo of the context in main.
    h.settle(300);
    assert!(!h.screen().contains("echo: [Context"));
    let main_text = std::fs::read_to_string(paths::transcript_path(&h.project, &h.main_id())).unwrap();
    assert!(!main_text.contains("Context from thread"));
    h.quit();
}

#[test]
fn clear_and_resume_switch_generations() {
    let mut h = H::new(160, 40, cfg());
    wait_ready(&mut h);
    h.say("gen one");
    let first_main = h.main_id();
    let t = h.new_thread("old thread");
    h.wait("thread live", |h| h.app.threads.get(&t).is_some_and(|s| s.live()));
    h.cmd('m');
    h.typ("/clear");
    h.enter();
    h.wait("new generation", |h| h.main_id() != first_main);
    assert!(h.app.visible_threads().is_empty());
    assert!(!h.app.threads.get(&t).is_some_and(|s| s.live()), "old thread discarded");
    assert_eq!(h.app.store.state.generations.len(), 2);
    assert!(!h.screen().contains("old thread"));
    // /compact (same id) changes nothing; /resume brings the threads back
    h.typ(&format!("/resume {first_main}"));
    h.enter();
    h.wait("back to gen one", |h| h.main_id() == first_main);
    assert_eq!(h.app.visible_threads().len(), 1);
    h.wait_text("old thread");
    h.quit();
}

#[test]
fn manage_threads_rename_reorder_archive_delete() {
    let mut h = H::new(160, 40, cfg());
    wait_ready(&mut h);
    h.say("x");
    let a = h.new_thread("aaa");
    h.cmd('m');
    let b = h.new_thread("bbb");
    // rename (FR-15): empty is refused, duplicates allowed
    h.cmd('r');
    h.key(KeyCode::Char('u'), KeyModifiers::CONTROL);
    h.enter();
    assert!(matches!(h.app.modal, Some(Modal::Rename { .. })));
    h.typ("aaa");
    h.enter();
    assert!(h.app.modal.is_none());
    assert_eq!(h.app.store.state.thread(&b).unwrap().title, "aaa");
    // reorder (FR-16)
    h.cmd('K');
    let order: Vec<_> = h.app.visible_threads().iter().map(|t| t.id.clone()).collect();
    assert_eq!(order, vec![b.clone(), a.clone()]);
    // number keys (FR-14)
    h.cmd('2');
    assert_eq!(h.app.open_thread(), Some(a.as_str()));
    // archive (FR-17)
    h.cmd('a');
    assert!(h.app.open_thread().is_none());
    assert_eq!(h.app.visible_threads().len(), 1);
    h.wait("archived discarded", |h| !h.app.threads.get(&a).is_some_and(|s| s.live()));
    h.cmd('A');
    h.wait_text("Archived threads");
    h.enter();
    assert_eq!(h.app.visible_threads().len(), 2);
    // delete (FR-18) via the sidebar
    h.cmd('h');
    assert_eq!(h.app.focus, Focus::Sidebar);
    h.ch('j');
    h.ch('D');
    assert!(matches!(h.app.modal, Some(Modal::ConfirmDelete { .. })));
    h.ch('y');
    assert_eq!(h.app.visible_threads().len(), 1);
    assert!(h.app.store.state.thread(&b).is_none());
    h.key(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(h.app.focus, Focus::Main);
    h.quit();
}

fn git(dir: &Path, args: &[&str]) {
    let s = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(s.status.success(), "{}", String::from_utf8_lossy(&s.stderr));
}

#[test]
fn worktree_thread_create_and_delete() {
    env();
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("shop");
    std::fs::create_dir(&project).unwrap();
    git(&project, &["init", "-q", "-b", "main"]);
    git(&project, &["config", "user.email", "t@example.com"]);
    git(&project, &["config", "user.name", "t"]);
    std::fs::write(project.join("README"), "x").unwrap();
    git(&project, &["add", "."]);
    git(&project, &["commit", "-q", "-m", "init"]);
    // dirty main folder → warning in the prompt (FR-53)
    std::fs::write(project.join("dirty.txt"), "x").unwrap();

    let mut h = H::open(&project, &tmp.path().join("state"), 160, 40, cfg());
    wait_ready(&mut h);
    h.say("plan db");
    h.cmd('n');
    assert!(h.screen().contains("Isolate in a git worktree"));
    assert!(h.screen().contains("Uncommitted changes in main"));
    h.key(KeyCode::Tab, KeyModifiers::NONE);
    h.ch(' ');
    assert!(h.screen().contains("[x] Isolate"));
    h.typ("DB schema");
    h.enter();
    let id = h.app.open_thread().unwrap().to_string();
    let t = h.app.store.state.thread(&id).unwrap().clone();
    let wt = t.worktree.clone().unwrap();
    assert_eq!(wt.branch, "cthread/db-schema");
    assert!(wt.path.ends_with("shop-db-schema"));
    assert_eq!(t.cwd, wt.path);
    assert!(!wt.path.join("dirty.txt").exists());
    h.wait_text("⎇ cthread/db-schema");
    // forked history available inside the worktree
    h.wait("history in worktree", |h| h.screen().matches("> plan db").count() >= 2);
    // edits in the worktree stay there
    std::fs::write(wt.path.join("schema.sql"), "create table x;").unwrap();
    let st = Command::new("git").arg("-C").arg(&h.project).args(["status", "--porcelain"]).output().unwrap();
    assert!(!String::from_utf8_lossy(&st.stdout).contains("schema.sql"));

    // delete: remove worktree → refused first because it's dirty, then confirmed
    h.cmd('D');
    h.ch('y');
    assert!(matches!(h.app.modal, Some(Modal::DeleteWorktree { confirm_dirty: false, .. })));
    h.ch('r');
    assert!(matches!(h.app.modal, Some(Modal::DeleteWorktree { confirm_dirty: true, .. })));
    h.ch('y');
    assert!(h.app.modal.is_none());
    h.wait("worktree removed", |_| !wt.path.exists());
    let br = Command::new("git").arg("-C").arg(&h.project).args(["branch", "--list", "cthread/*"]).output().unwrap();
    assert!(String::from_utf8_lossy(&br.stdout).trim().is_empty());
    h.quit();
}

#[test]
fn relaunch_restores_main_threads_and_layout() {
    let mut h = H::new(160, 40, cfg());
    wait_ready(&mut h);
    h.say("remember me");
    let t = h.new_thread("kept");
    h.say("in thread");
    h.cmd('>');
    let ratio = h.app.store.state.ui.split_ratio;
    assert!(ratio < 0.5);
    let main_id = h.main_id();
    let (project, state_dir) = (h.project.clone(), h.state_dir.clone());
    let tmp = h._tmp.take();
    h.quit();

    let mut h = H::open(&project, &state_dir, 160, 40, cfg());
    h._tmp = tmp;
    wait_ready(&mut h);
    assert_eq!(h.main_id(), main_id);
    h.wait_text("> remember me");
    assert_eq!(h.app.store.state.ui.split_ratio, ratio);
    assert_eq!(h.app.open_thread(), Some(t.as_str()));
    h.wait_text("echo: in thread");
    h.quit();
}

#[test]
fn main_exit_and_restart() {
    let mut h = H::new(160, 40, cfg());
    wait_ready(&mut h);
    h.say("before crash");
    h.typ("crash");
    h.enter();
    h.wait_text("main exited — press Enter to restart");
    h.enter();
    h.wait("restarted", |h| !h.app.main_exited && h.screen().contains("> before crash"));
    h.quit();
}

#[test]
fn main_exit_before_any_message_restarts_fresh() {
    let mut h = H::new(160, 40, cfg());
    wait_ready(&mut h);
    let first = h.main_id();
    h.typ("crash");
    h.enter();
    h.wait_text("main exited — press Enter to restart");
    h.enter();
    // --resume would fail: there is no transcript yet. A fresh session starts.
    h.wait("restarted fresh", |h| !h.app.main_exited && h.screen().contains("fake-claude") && h.main_id() != first);
    h.settle(300);
    assert!(!h.app.main_exited);
    assert!(!h.app.store.state.generations.contains_key(&first));
    h.say("works now");
    h.quit();
}

#[test]
fn narrow_mode_and_too_small() {
    let mut h = H::new(100, 30, cfg());
    wait_ready(&mut h);
    h.say("n1");
    let _t = h.new_thread("narrow one");
    // only the thread is visible, sidebar collapsed to glyphs
    assert!(h.app.layout.narrow);
    assert!(h.app.layout.main.is_none());
    assert_eq!(h.app.layout.sidebar.unwrap().width, 3);
    h.cmd('m');
    assert!(h.app.layout.thread.is_none());
    h.cmd('b');
    assert!(h.app.layout.overlay.is_some());
    assert!(h.screen().contains("narrow one"));
    h.key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(h.app.layout.overlay.is_none());
    h.app.handle(AppEvent::Term(Event::Resize(50, 10)));
    h.step();
    assert!(h.screen().contains("terminal too small"));
    h.quit();
}

/// A custom command (wrapper script + leading args, given as a string) is
/// used for sessions, and `pull_command` separately for summaries.
#[test]
fn custom_claude_command() {
    let tmp = tempfile::tempdir().unwrap();
    let wrapper = tmp.path().join("my claude.sh");
    let log = tmp.path().join("calls.log");
    let script = format!(
        "#!/bin/sh\necho \"$1 $2\" >> '{}'\nshift 2\nexec '{}' \"$@\"\n",
        log.display(),
        env().fake_claude.display()
    );
    std::fs::write(&wrapper, script).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut c = cfg();
    let quoted = shell_words::quote(&wrapper.to_string_lossy()).to_string();
    c.claude.command = Some(CommandLine::Line(format!("{quoted} --profile 'my work'")));
    c.claude.pull_command = Some(CommandLine::Line(format!("{quoted} --profile pull")));
    let mut h = H::new(160, 40, c);
    wait_ready(&mut h);
    h.say("via wrapper");
    h.new_thread("wrapped thread");
    h.say("in thread");
    h.cmd('p');
    h.wait("summary", |h| matches!(h.app.modal, Some(Modal::Pull { phase: PullPhase::Ready, .. })));
    let calls = std::fs::read_to_string(&log).unwrap();
    // main + thread used the session command; the pull used pull_command.
    assert_eq!(calls.matches("--profile my work").count(), 2, "{calls}");
    assert_eq!(calls.matches("--profile pull").count(), 1, "{calls}");
    h.quit();
}

#[test]
fn custom_prefix_and_command_keys() {
    let (mut c, w) = Config::parse(
        "[keys]\nprefix = \"C-a\"\n[keys.commands]\nnew_thread = \"t\"\nclose = \"C-w\"\nhelp = \"H\"\n",
    );
    assert!(w.is_empty(), "{w:?}");
    let base = cfg();
    c.claude = base.claude;
    c.pull = base.pull;
    let mut h = H::new(160, 40, c);
    wait_ready(&mut h);
    h.wait_text("^a t new");
    h.say("hello");

    // the old prefix and the old command key do nothing special
    h.key(KeyCode::Char(']'), KeyModifiers::CONTROL);
    h.ch('n');
    assert!(h.app.modal.is_none());
    h.key(KeyCode::Char('a'), KeyModifiers::CONTROL);
    h.ch('n');
    assert!(h.app.modal.is_none(), "n is no longer new_thread");

    // prefix C-a, then t: new thread
    h.key(KeyCode::Char('a'), KeyModifiers::CONTROL);
    h.ch('t');
    assert!(matches!(h.app.modal, Some(Modal::NewThread { .. })), "{}", h.screen());
    h.typ("rebound");
    h.enter();
    h.wait_text("# rebound");

    // C-a C-w closes it; C-a H opens help, which lists the new keys
    h.key(KeyCode::Char('a'), KeyModifiers::CONTROL);
    h.key(KeyCode::Char('w'), KeyModifiers::CONTROL);
    assert!(h.app.open_thread().is_none());
    h.key(KeyCode::Char('a'), KeyModifiers::CONTROL);
    h.ch('H');
    assert!(matches!(h.app.modal, Some(Modal::Help)));
    h.wait_text("^a t ");
    h.wait_text("^a ^w ");
    h.quit();
}

#[test]
fn threads_drop_background_tasks_and_can_be_read_only() {
    let mut h = H::new(240, 40, cfg());
    wait_ready(&mut h);
    // main keeps every tool
    assert!(!h.screen().contains("disallowed="), "{}", h.screen());
    h.say("hello");

    // default thread: read-write, minus subagents/Monitor, background tasks off
    let rw = h.new_thread("rw");
    h.wait_text("[disallowed=Agent,Monitor,Workflow bg=off]");
    assert!(!h.app.store.state.thread(&rw).unwrap().read_only);
    h.cmd('x');

    // read-only thread: Tab to the checkbox, Space to tick it, then type the title
    h.cmd('n');
    h.key(KeyCode::Tab, KeyModifiers::NONE);
    h.ch(' ');
    assert!(h.screen().contains("[x] Read-only tools"), "{}", h.screen());
    assert!(h.screen().contains("Only Read, Grep, Glob, WebFetch, WebSearch (no MCP)"));
    h.typ("ro");
    h.enter();
    let ro = h.app.open_thread().unwrap().to_string();
    h.wait_text("[tools=Read,Grep,Glob,WebFetch,WebSearch disallowed=Agent,Monitor,Workflow");
    h.wait_text("no-mcp bg=");
    h.wait_text("read-only");
    assert!(h.app.store.state.thread(&ro).unwrap().read_only);
    h.say("in ro");

    // after a restart the restored thread gets the same limits again
    let (project, state_dir) = (h.project.clone(), h.state_dir.clone());
    let keep = h._tmp.take();
    h.quit();
    let mut h = H::open(&project, &state_dir, 240, 40, cfg());
    wait_ready(&mut h);
    assert_eq!(h.app.open_thread(), Some(ro.as_str()));
    h.wait_text("[tools=Read,Grep,Glob,WebFetch,WebSearch disallowed=Agent,Monitor,Workflow");
    h.wait_text("no-mcp bg=");
    h.quit();
    drop(keep);
}
