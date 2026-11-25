//! Application state and behaviour. Rendering lives in `ui.rs`; the terminal
//! event loop lives in `main.rs` (or in tests, driven headless).

use crate::config::Config;
use crate::hooks::{HookEvent, HookMsg};
use crate::keys::{self, KeyOutcome, Prefix, PrefixMachine};
use crate::layout::{self, Layout, LayoutInput};
use crate::pane::{Pane, PaneNotice, SpawnSpec};
use crate::paths::{self, ProjectPaths};
use crate::policy::{self, Activity, Candidate, Status};
use crate::state::{GenChange, Store, ThreadRec, WorktreeRec};
use crate::summarizer::{self, SummaryRequest, SummaryResult};
use crate::textarea::TextArea;
use crate::{git, text};
use anyhow::{Context, Result};
use chrono::Utc;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const STOP_GRACE: Duration = Duration::from_secs(3);
const STATUS_TTL: Duration = Duration::from_secs(4);
const SPLIT_STEP: f32 = 0.05;

/// Everything the event loop feeds into the app.
#[derive(Debug)]
pub enum AppEvent {
    Term(Event),
    Pane(PaneNotice),
    Hook(HookMsg),
    Summary(SummaryResult),
}

pub type Notify = Arc<dyn Fn(AppEvent) + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Main,
    Thread,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owner {
    Main,
    Thread(String),
}

/// Runtime (non-persisted) data for one session.
pub struct Session {
    pub pane: Option<Pane>,
    pub activity: Activity,
    pub last_viewed: Instant,
    pub last_active: Instant,
    /// Waiting for the first SessionStart after spawn.
    pub first_start: bool,
    /// Placeholder text while the PTY has produced no output yet.
    pub placeholder: &'static str,
}

impl Session {
    pub fn new() -> Self {
        let now = Instant::now();
        Session { pane: None, activity: Activity::Idle, last_viewed: now, last_active: now, first_start: false, placeholder: "" }
    }
    pub fn live(&self) -> bool {
        self.pane.is_some()
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PullPhase {
    Loading,
    Ready,
    Failed(String),
}

pub enum Modal {
    NewThread { input: TextArea, worktree: bool, git_ok: bool, dirty: bool, hint: Option<String> },
    Rename { id: String, input: TextArea, hint: Option<String> },
    Pull { thread: String, title: String, job: u64, cancel: Arc<AtomicBool>, phase: PullPhase, text: TextArea, editing: bool },
    Archived { selected: usize },
    ConfirmDelete { id: String },
    DeleteWorktree { id: String, confirm_dirty: bool },
    ConfirmQuit,
    Help,
}

/// Request for the runner to suspend the TUI and open `$EDITOR`.
#[derive(Debug, Clone)]
pub struct EditorRequest {
    pub text: String,
}

pub struct AppOptions {
    /// Path to the cthread binary used for the hook command.
    pub hook_exe: PathBuf,
}

pub struct App {
    pub cfg: Config,
    pub paths: ProjectPaths,
    pub store: Store,
    pub prefix: PrefixMachine,
    pub notify: Notify,

    pub main: Session,
    pub main_unread: bool,
    pub main_exited: bool,
    pub main_transcript: Option<PathBuf>,
    pub threads: HashMap<String, Session>,
    pane_owner: HashMap<String, Owner>,

    pub focus: Focus,
    prev_focus: Focus,
    /// Sidebar selection: 0 = main, i = i-th visible thread (1-based).
    pub sidebar_sel: usize,
    pub narrow_show_thread: bool,
    pub overlay: bool,
    pub modal: Option<Modal>,
    pub status_msg: Option<(String, Instant)>,
    pub size: (u16, u16),
    pub layout: Layout,
    dragging: bool,
    pub frame: usize,
    next_job: u64,
    pub editor_request: Option<EditorRequest>,
    pub bell: bool,
    pub quit: bool,
    pub dirty: bool,
    last_tick: Instant,
}

fn now_utc() -> chrono::DateTime<Utc> {
    Utc::now()
}

impl App {
    pub fn new(cfg: Config, paths: ProjectPaths, store: Store, opts: AppOptions, notify: Notify) -> Result<App> {
        let prefix = Prefix::parse(&cfg.keys.prefix).unwrap_or(Prefix { ch: ']', ctrl: true, alt: false });
        std::fs::create_dir_all(&paths.state_dir).ok();
        std::fs::write(&paths.settings_file, crate::hooks::settings_json(&opts.hook_exe))
            .with_context(|| format!("writing {}", paths.settings_file.display()))?;
        {
            let n = notify.clone();
            crate::hooks::listen(&paths.socket, move |m| n(AppEvent::Hook(m)))
                .with_context(|| format!("listening on {}", paths.socket.display()))?;
        }
        Ok(App {
            cfg,
            paths,
            store,
            prefix: PrefixMachine::new(prefix),
            notify,
            main: Session::new(),
            main_unread: false,
            main_exited: false,
            main_transcript: None,
            threads: HashMap::new(),
            pane_owner: HashMap::new(),
            focus: Focus::Main,
            prev_focus: Focus::Main,
            sidebar_sel: 0,
            narrow_show_thread: true,
            overlay: false,
            modal: None,
            status_msg: None,
            size: (0, 0),
            layout: Layout::default(),
            dragging: false,
            frame: 0,
            next_job: 1,
            editor_request: None,
            bell: false,
            quit: false,
            dirty: true,
            last_tick: Instant::now(),
        })
    }

    // ---------------------------------------------------------------- basics

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status_msg = Some((msg.into(), Instant::now()));
        self.dirty = true;
    }

    pub fn status_text(&self, now: Instant) -> Option<&str> {
        self.status_msg.as_ref().filter(|(_, t)| now.duration_since(*t) < STATUS_TTL).map(|(m, _)| m.as_str())
    }

    pub fn open_thread(&self) -> Option<&str> {
        self.store.state.active()?.open_thread.as_deref()
    }

    /// Non-archived threads in sidebar order.
    pub fn visible_threads(&self) -> Vec<&ThreadRec> {
        self.store.state.threads().iter().filter(|t| !t.archived).collect()
    }

    pub fn archived_threads(&self) -> Vec<&ThreadRec> {
        self.store.state.threads().iter().filter(|t| t.archived).collect()
    }

    pub fn thread_status(&self, t: &ThreadRec) -> Status {
        match self.threads.get(&t.id) {
            Some(s) => policy::status(s.activity, t.unread, s.live()),
            None => policy::status(Activity::Idle, t.unread, false),
        }
    }

    pub fn main_status(&self) -> Status {
        policy::status(self.main.activity, self.main_unread, self.main.live() && !self.main_exited)
    }

    fn layout_input(&self, thread_open: bool) -> LayoutInput {
        LayoutInput {
            area: Rect::new(0, 0, self.size.0, self.size.1),
            sidebar_width: self.cfg.ui.sidebar_width,
            sidebar_visible: self.store.state.ui.sidebar_visible,
            narrow_breakpoint: self.cfg.ui.narrow_breakpoint,
            thread_open,
            narrow_show_thread: self.narrow_show_thread,
            overlay: self.overlay,
            split_ratio: self.store.state.ui.split_ratio,
        }
    }

    pub fn compute_layout(&self) -> Layout {
        layout::compute(&self.layout_input(self.open_thread().is_some()))
    }

    /// Whether a session currently has the user's attention (for Unread).
    fn is_focused(&self, owner: &Owner) -> bool {
        let narrow = self.layout.narrow;
        match owner {
            Owner::Main => self.focus == Focus::Main && (!narrow || !self.narrow_show_thread || self.open_thread().is_none()),
            Owner::Thread(id) => self.focus == Focus::Thread && self.open_thread() == Some(id.as_str()),
        }
    }

    fn session_mut(&mut self, owner: &Owner) -> Option<&mut Session> {
        match owner {
            Owner::Main => Some(&mut self.main),
            Owner::Thread(id) => self.threads.get_mut(id),
        }
    }

    pub fn focused_pane(&self) -> Option<&Pane> {
        match self.focus {
            Focus::Main => self.main.pane.as_ref(),
            Focus::Thread => self.open_thread().and_then(|id| self.threads.get(id)).and_then(|s| s.pane.as_ref()),
            Focus::Sidebar => None,
        }
    }

    fn base_args(&self) -> Vec<String> {
        let mut a = vec!["--settings".to_string(), self.paths.settings_file.to_string_lossy().to_string()];
        a.extend(self.cfg.claude.extra_args.iter().cloned());
        a
    }

    fn spawn(&mut self, owner: Owner, cwd: PathBuf, args: Vec<String>, rows: u16, cols: u16) -> Result<Pane> {
        // <claude.command…> <cthread's session flags> --settings … <extra_args>
        let mut argv = self.cfg.claude.argv();
        let program = argv.remove(0);
        argv.extend(args);
        argv.extend(self.base_args());
        let spec = SpawnSpec {
            program,
            args: argv,
            cwd,
            env: vec![("CTHREAD_SOCK".into(), self.paths.socket.to_string_lossy().to_string())],
        };
        let n = self.notify.clone();
        crate::dlog!("spawn {:?} {} {:?} in {} ({rows}x{cols})", owner, spec.program, spec.args, spec.cwd.display());
        let pane = Pane::spawn(&spec, rows, cols, Arc::new(move |p| n(AppEvent::Pane(p))))?;
        crate::dlog!("  pane {}", pane.id);
        self.pane_owner.insert(pane.id.clone(), owner);
        Ok(pane)
    }

    // ------------------------------------------------------------- lifecycle

    /// Start main (FR-1) and restore the open thread. `size` is the terminal size.
    pub fn start(&mut self, size: (u16, u16)) -> Result<()> {
        self.size = size;
        self.layout = self.compute_layout();
        self.spawn_main()?;
        if let Some(id) = self.open_thread().map(str::to_string) {
            if self.store.state.thread(&id).is_some_and(|t| !t.archived) {
                self.ensure_live(&id);
                self.focus = Focus::Main;
            } else if let Some(g) = self.store.state.active_mut() {
                g.open_thread = None;
            }
        }
        Ok(())
    }

    fn spawn_main(&mut self) -> Result<()> {
        let pty = self.layout.main_pty;
        let (rows, cols) = (pty.height.max(5), pty.width.max(20));
        // A main that never saved a message has no transcript, and
        // `--resume` would fail ("No conversation found"): start it fresh.
        if let Some(id) = self.store.state.active_main.clone() {
            let saved = paths::transcript_path(&self.paths.project_dir, &id).is_file() || paths::find_transcript(&id).is_some();
            let empty = self.store.state.active().is_none_or(|g| g.threads.is_empty());
            if !saved && empty {
                self.store.state.generations.remove(&id);
                self.store.state.active_main = None;
            }
        }
        let args = match self.store.state.active_main.clone() {
            Some(id) => vec!["--resume".into(), id],
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                self.store.state.start_generation(&id, now_utc());
                self.store.mark_dirty();
                vec!["--session-id".into(), id]
            }
        };
        let pane = self.spawn(Owner::Main, self.paths.project_dir.clone(), args, rows, cols)?;
        self.main.pane = Some(pane);
        self.main.first_start = true;
        self.main.activity = Activity::Idle;
        self.main.placeholder = "Starting Claude Code…";
        self.main_exited = false;
        Ok(())
    }

    /// Make sure a thread has a running process; restore it if discarded (FR-32).
    fn ensure_live(&mut self, id: &str) {
        if self.threads.get(id).is_some_and(|s| s.live()) {
            return;
        }
        let Some(t) = self.store.state.thread(id).cloned() else { return };
        let Some(sid) = t.resume_id().map(str::to_string) else {
            self.set_status(format!("Thread \"{}\" has no session to restore", t.title));
            return;
        };
        let pty = self.compute_thread_pty();
        match self.spawn(Owner::Thread(id.to_string()), t.cwd.clone(), vec!["--resume".into(), sid], pty.height, pty.width) {
            Ok(p) => {
                let s = self.threads.entry(id.to_string()).or_default();
                s.pane = Some(p);
                s.activity = Activity::Idle;
                s.placeholder = "Restoring…";
                s.last_active = Instant::now();
            }
            Err(e) => self.set_status(format!("Could not restore thread: {e:#}")),
        }
    }

    fn compute_thread_pty(&self) -> Rect {
        let r = layout::compute(&LayoutInput { narrow_show_thread: true, ..self.layout_input(true) }).thread_pty;
        Rect { height: r.height.max(5), width: r.width.max(20), ..r }
    }

    /// Stop a thread's process and keep its record (discard / archive).
    fn discard(&mut self, id: &str) {
        if let Some(s) = self.threads.get_mut(id) {
            if let Some(p) = s.pane.take() {
                self.pane_owner.remove(&p.id);
                p.stop(STOP_GRACE);
            }
            s.activity = Activity::Idle;
        }
        self.dirty = true;
    }

    /// Stop everything (quit). Blocks up to the grace period.
    pub fn shutdown(&mut self) {
        let mut panes: Vec<Pane> = self.threads.values_mut().filter_map(|s| s.pane.take()).collect();
        if let Some(p) = self.main.pane.take() {
            panes.push(p);
        }
        self.pane_owner.clear();
        let handles: Vec<_> = panes.into_iter().map(|p| std::thread::spawn(move || p.stop_blocking(STOP_GRACE))).collect();
        for h in handles {
            let _ = h.join();
        }
        if let Some(Modal::Pull { cancel, .. }) = &self.modal {
            cancel.store(true, Ordering::Relaxed);
        }
        let _ = self.store.save_now();
    }

    // ---------------------------------------------------------------- events

    pub fn handle(&mut self, ev: AppEvent) {
        match &ev {
            AppEvent::Pane(PaneNotice::Output(_)) | AppEvent::Term(_) => {}
            e => crate::dlog!("event {e:?}"),
        }
        match ev {
            AppEvent::Term(e) => self.on_term(e),
            AppEvent::Pane(PaneNotice::Output(id)) => {
                if let Some(Owner::Thread(t)) = self.pane_owner.get(&id)
                    && let Some(s) = self.threads.get_mut(t) {
                        s.last_active = Instant::now();
                    }
                self.dirty = true;
            }
            AppEvent::Pane(PaneNotice::Exited(id)) => self.on_exit(&id),
            AppEvent::Hook(m) => self.on_hook(m),
            AppEvent::Summary(r) => self.on_summary(r),
        }
    }

    fn on_exit(&mut self, pane_id: &str) {
        let Some(owner) = self.pane_owner.remove(pane_id) else { return };
        match owner {
            Owner::Main => {
                // Keep the pane so its last screen stays visible (FR-63).
                self.main_exited = true;
                self.main.activity = Activity::Idle;
            }
            Owner::Thread(id) => {
                if let Some(s) = self.threads.get_mut(&id) {
                    s.pane = None;
                    s.activity = Activity::Idle;
                }
                if self.open_thread() == Some(id.as_str()) {
                    self.set_status("Thread process exited — reopen it to restore");
                }
            }
        }
        self.dirty = true;
    }

    fn on_hook(&mut self, m: HookMsg) {
        let Some(owner) = self.pane_owner.get(&m.pane).cloned() else { return };
        let focused = self.is_focused(&owner);
        let now = Instant::now();
        match m.event {
            HookEvent::SessionStart { session_id, source, transcript_path, .. } => match &owner {
                Owner::Main => {
                    let first = std::mem::take(&mut self.main.first_start);
                    if let Some(p) = transcript_path {
                        self.main_transcript = Some(PathBuf::from(p));
                    }
                    let change = self.store.state.on_main_session_start(&session_id, &source, first, now_utc());
                    self.store.mark_dirty();
                    match change {
                        GenChange::New { old: Some(_) } | GenChange::Switched { .. } => self.on_generation_switch(),
                        _ => {}
                    }
                }
                Owner::Thread(id) => {
                    // FR-62: follow session-ID changes inside the thread.
                    if let Some(t) = self.store.state.thread_mut(id) {
                        if t.session_id.as_deref() != Some(session_id.as_str()) {
                            t.session_id = Some(session_id);
                        }
                        t.pending_session_id = None;
                        self.store.mark_dirty();
                    }
                }
            },
            HookEvent::UserPromptSubmit { .. } => {
                if let Some(s) = self.session_mut(&owner) {
                    s.activity = Activity::Working;
                    s.last_active = now;
                }
                if let Owner::Thread(id) = &owner
                    && let Some(t) = self.store.state.thread_mut(id) {
                        t.turns += 1;
                        self.store.mark_dirty();
                    }
            }
            HookEvent::Stop { .. } => {
                if let Some(s) = self.session_mut(&owner) {
                    s.activity = Activity::Idle;
                    s.last_active = now;
                }
                if !focused {
                    self.set_unread(&owner, true);
                }
            }
            HookEvent::Notification { needs_you, .. } => {
                if needs_you {
                    let was = self.session_mut(&owner).map(|s| s.activity);
                    if let Some(s) = self.session_mut(&owner) {
                        s.activity = Activity::NeedsYou;
                        s.last_active = now;
                    }
                    if was != Some(Activity::NeedsYou) && self.cfg.ui.bell_on_needs_you {
                        self.bell = true;
                    }
                }
            }
            HookEvent::SessionEnd { .. } | HookEvent::Other(_) => {}
        }
        self.dirty = true;
    }

    fn set_unread(&mut self, owner: &Owner, v: bool) {
        match owner {
            Owner::Main => self.main_unread = v,
            Owner::Thread(id) => {
                if let Some(t) = self.store.state.thread_mut(id)
                    && t.unread != v {
                        t.unread = v;
                        self.store.mark_dirty();
                    }
            }
        }
    }

    /// Main moved to another generation (FR-60, FR-61): discard every thread
    /// process that is not part of the new active generation.
    fn on_generation_switch(&mut self) {
        let active: Vec<String> = self.store.state.threads().iter().map(|t| t.id.clone()).collect();
        let stale: Vec<String> = self.threads.keys().filter(|id| !active.contains(id)).cloned().collect();
        for id in stale {
            self.discard(&id);
            self.threads.remove(&id);
        }
        if let Some(Modal::Pull { cancel, .. }) = &self.modal {
            cancel.store(true, Ordering::Relaxed);
        }
        self.modal = None;
        if let Some(g) = self.store.state.active_mut() {
            g.open_thread = None;
        }
        self.focus = Focus::Main;
        self.sidebar_sel = 0;
        self.main_unread = false;
    }

    fn on_summary(&mut self, r: SummaryResult) {
        if let Some(Modal::Pull { job, phase, text, .. }) = &mut self.modal
            && *job == r.job {
                match r.result {
                    Ok(s) => {
                        text.set(&s);
                        *phase = PullPhase::Ready;
                    }
                    Err(e) => *phase = PullPhase::Failed(e),
                }
                self.dirty = true;
            }
    }

    /// Timers: discard policy, debounced saves, spinner. Call often.
    pub fn tick(&mut self, now: Instant) {
        if now.duration_since(self.last_tick) >= Duration::from_millis(150) {
            self.last_tick = now;
            let animating = self.main.activity == Activity::Working
                || self.threads.values().any(|s| s.activity == Activity::Working && s.live())
                || matches!(self.modal, Some(Modal::Pull { phase: PullPhase::Loading, .. }));
            if animating {
                self.frame = self.frame.wrapping_add(1);
                self.dirty = true;
            }
            self.run_discard_policy(now);
        }
        if self.prefix.is_armed() && !self.prefix.hint_visible(now) {
            self.dirty = true;
        }
        if let Some((_, t)) = &self.status_msg
            && now.duration_since(*t) >= STATUS_TTL {
                self.status_msg = None;
                self.dirty = true;
            }
        let _ = self.store.maybe_save(now);
    }

    fn run_discard_policy(&mut self, now: Instant) {
        let open = self.open_thread().map(str::to_string);
        let cands: Vec<Candidate> = self
            .threads
            .iter()
            .filter(|(_, s)| s.live())
            .map(|(id, s)| Candidate {
                id: id.clone(),
                open: open.as_deref() == Some(id.as_str()),
                activity: s.activity,
                last_viewed: s.last_viewed,
                last_active: s.last_active.max(s.last_viewed),
            })
            .collect();
        let after = Duration::from_secs(self.cfg.threads.discard_after_minutes * 60);
        for id in policy::choose_discards(&cands, now, after, self.cfg.threads.max_live) {
            self.discard(&id);
        }
    }

    /// Resize PTYs to match the current layout. Call after each draw.
    pub fn sync_sizes(&mut self) {
        let l = self.compute_layout();
        if l.too_small {
            self.layout = l;
            return;
        }
        if let Some(p) = self.main.pane.as_mut() {
            p.resize(l.main_pty.height, l.main_pty.width);
        }
        if let Some(id) = self.open_thread().map(str::to_string)
            && let Some(p) = self.threads.get_mut(&id).and_then(|s| s.pane.as_mut()) {
                p.resize(l.thread_pty.height, l.thread_pty.width);
            }
        self.layout = l;
    }

    // ---------------------------------------------------------------- input

    fn on_term(&mut self, e: Event) {
        self.dirty = true;
        match e {
            Event::Key(k) => self.on_key(k),
            Event::Paste(s) => self.on_paste(&s),
            Event::Mouse(m) => self.on_mouse(m),
            Event::Resize(w, h) => {
                self.size = (w, h);
                self.sync_sizes();
            }
            _ => {}
        }
    }

    fn on_paste(&mut self, s: &str) {
        if let Some(m) = &mut self.modal {
            match m {
                Modal::NewThread { input, .. } | Modal::Rename { input, .. } => input.insert_str(s),
                Modal::Pull { text, editing: true, .. } => text.insert_str(s),
                _ => {}
            }
            return;
        }
        if let Some(p) = self.focused_pane() {
            if p.parser().screen().bracketed_paste() {
                p.input(&keys::bracketed(s));
            } else {
                p.input(s.as_bytes());
            }
        }
    }

    fn on_key(&mut self, k: KeyEvent) {
        if k.kind == crossterm::event::KeyEventKind::Release {
            return;
        }
        if self.modal.is_some() {
            self.on_modal_key(k);
            return;
        }
        match self.prefix.feed(k) {
            KeyOutcome::Ignore | KeyOutcome::Armed => {}
            KeyOutcome::Command(c) => self.on_command(c),
            KeyOutcome::LiteralPrefix => {
                let b = self.prefix.prefix.bytes();
                self.send_to_focused(&b);
            }
            KeyOutcome::Forward(k) => {
                if self.focus == Focus::Sidebar {
                    self.on_sidebar_key(k);
                } else {
                    self.forward_key(k);
                }
            }
        }
    }

    fn send_to_focused(&mut self, bytes: &[u8]) {
        let owner = match self.focus {
            Focus::Main => Owner::Main,
            Focus::Thread => match self.open_thread() {
                Some(id) => Owner::Thread(id.to_string()),
                None => return,
            },
            Focus::Sidebar => return,
        };
        if let Some(p) = self.focused_pane() {
            p.input(bytes);
        }
        // Answering a permission prompt resumes work.
        if let Some(s) = self.session_mut(&owner) {
            if s.activity == Activity::NeedsYou {
                s.activity = Activity::Working;
            }
            s.last_active = Instant::now();
        }
    }

    fn forward_key(&mut self, k: KeyEvent) {
        if self.focus == Focus::Main && self.main_exited {
            if k.code == KeyCode::Enter {
                self.restart_main();
            }
            return;
        }
        let Some(p) = self.focused_pane() else { return };
        let bytes = keys::encode_key(&k, p.app_cursor());
        self.send_to_focused(&bytes);
    }

    fn restart_main(&mut self) {
        if let Some(p) = self.main.pane.take() {
            self.pane_owner.remove(&p.id);
            p.stop(STOP_GRACE);
        }
        if let Err(e) = self.spawn_main() {
            self.set_status(format!("Could not restart main: {e:#}"));
        }
    }

    /// The thread a command acts on: the sidebar selection when the sidebar
    /// is focused, else the open thread.
    fn target_thread(&self) -> Option<String> {
        if self.focus == Focus::Sidebar && self.sidebar_sel > 0 {
            return self.visible_threads().get(self.sidebar_sel - 1).map(|t| t.id.clone());
        }
        self.open_thread().map(str::to_string)
    }

    fn on_command(&mut self, k: KeyEvent) {
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let KeyCode::Char(c) = k.code else {
            return; // unknown key cancels silently
        };
        let c = if shift && c.is_ascii_lowercase() { c.to_ascii_uppercase() } else { c };
        match c {
            'n' => self.cmd_new_thread(),
            'p' => self.cmd_pull(),
            'x' => self.close_thread(),
            'h' => self.move_focus(-1),
            'l' => self.move_focus(1),
            'j' => self.cycle_thread(1),
            'k' => self.cycle_thread(-1),
            '1'..='9' => {
                let i = c as usize - '1' as usize;
                if let Some(id) = self.visible_threads().get(i).map(|t| t.id.clone()) {
                    self.open(&id);
                }
            }
            'm' => self.focus_main(),
            'r' => self.cmd_rename(),
            'J' => self.move_thread(1),
            'K' => self.move_thread(-1),
            'a' => self.cmd_archive(),
            'A' => self.modal = Some(Modal::Archived { selected: 0 }),
            'D' => self.cmd_delete(),
            '<' => self.resize_split(SPLIT_STEP),
            '>' => self.resize_split(-SPLIT_STEP),
            'b' => self.toggle_sidebar(),
            '?' => self.modal = Some(Modal::Help),
            'q' => self.cmd_quit(),
            _ => {}
        }
    }

    fn on_sidebar_key(&mut self, k: KeyEvent) {
        let n = self.visible_threads().len();
        match k.code {
            KeyCode::Char('j') | KeyCode::Down => self.sidebar_sel = (self.sidebar_sel + 1).min(n),
            KeyCode::Char('k') | KeyCode::Up => self.sidebar_sel = self.sidebar_sel.saturating_sub(1),
            KeyCode::Enter => {
                if self.sidebar_sel == 0 {
                    self.focus_main();
                } else if let Some(id) = self.visible_threads().get(self.sidebar_sel - 1).map(|t| t.id.clone()) {
                    self.open(&id);
                }
            }
            KeyCode::Char('r') => self.cmd_rename(),
            KeyCode::Char('a') => self.cmd_archive(),
            KeyCode::Char('n') => self.cmd_new_thread(),
            KeyCode::Char('p') => self.cmd_pull(),
            KeyCode::Char('D') => self.cmd_delete(),
            KeyCode::Char('J') => self.move_thread(1),
            KeyCode::Char('K') => self.move_thread(-1),
            KeyCode::Esc => {
                self.overlay = false;
                self.focus = if self.prev_focus == Focus::Sidebar { Focus::Main } else { self.prev_focus };
                if self.focus == Focus::Thread && self.open_thread().is_none() {
                    self.focus = Focus::Main;
                }
                self.view_focused();
            }
            _ => {}
        }
    }

    // -------------------------------------------------------------- commands

    fn set_focus(&mut self, f: Focus) {
        if f == Focus::Sidebar && self.focus != Focus::Sidebar {
            self.prev_focus = self.focus;
            self.sidebar_sel = match self.open_thread() {
                Some(id) => self.visible_threads().iter().position(|t| t.id == id).map(|i| i + 1).unwrap_or(0),
                None => 0,
            };
        }
        if f != Focus::Sidebar {
            self.overlay = false;
        }
        self.focus = f;
        self.view_focused();
    }

    /// Mark the focused session as viewed: clears Unread.
    fn view_focused(&mut self) {
        let now = Instant::now();
        match self.focus {
            Focus::Main => {
                self.main_unread = false;
                self.main.last_viewed = now;
                if self.layout.narrow {
                    self.narrow_show_thread = false;
                }
            }
            Focus::Thread => {
                if let Some(id) = self.open_thread().map(str::to_string) {
                    if let Some(s) = self.threads.get_mut(&id) {
                        s.last_viewed = now;
                    }
                    if let Some(t) = self.store.state.thread_mut(&id) {
                        t.last_viewed_at = Some(now_utc());
                    }
                    self.set_unread(&Owner::Thread(id), false);
                    self.store.mark_dirty();
                    self.narrow_show_thread = true;
                }
            }
            Focus::Sidebar => {}
        }
        self.dirty = true;
    }

    pub fn focus_main(&mut self) {
        self.narrow_show_thread = false;
        self.set_focus(Focus::Main);
    }

    fn move_focus(&mut self, dir: i32) {
        let mut order = vec![];
        if self.layout.narrow || self.store.state.ui.sidebar_visible {
            order.push(Focus::Sidebar);
        }
        if self.layout.narrow {
            order.push(if self.open_thread().is_some() && self.narrow_show_thread { Focus::Thread } else { Focus::Main });
        } else {
            order.push(Focus::Main);
            if self.open_thread().is_some() {
                order.push(Focus::Thread);
            }
        }
        let cur = order.iter().position(|f| *f == self.focus).unwrap_or(0) as i32;
        let next = (cur + dir).clamp(0, order.len() as i32 - 1) as usize;
        let f = order[next];
        if f == Focus::Sidebar && self.layout.narrow {
            self.overlay = true;
        }
        self.set_focus(f);
    }

    /// Open a thread in the right pane (FR-13, FR-14).
    pub fn open(&mut self, id: &str) {
        if self.store.state.thread(id).is_none() {
            return;
        }
        if let Some(g) = self.store.state.active_mut() {
            g.open_thread = Some(id.to_string());
        }
        self.store.mark_dirty();
        self.narrow_show_thread = true;
        self.ensure_live(id);
        self.set_focus(Focus::Thread);
        self.sync_sizes();
    }

    pub fn close_thread(&mut self) {
        if let Some(g) = self.store.state.active_mut()
            && g.open_thread.take().is_some() {
                self.store.mark_dirty();
            }
        self.focus_main();
        self.sync_sizes();
    }

    fn cycle_thread(&mut self, dir: i32) {
        let ids: Vec<String> = self.visible_threads().iter().map(|t| t.id.clone()).collect();
        if ids.is_empty() {
            self.set_status("No threads yet");
            return;
        }
        let cur = self.open_thread().and_then(|o| ids.iter().position(|i| i == o));
        let n = ids.len() as i32;
        let next = match cur {
            Some(c) => (c as i32 + dir).rem_euclid(n),
            None if dir > 0 => 0,
            None => n - 1,
        };
        let id = ids[next as usize].clone();
        self.open(&id);
    }

    fn move_thread(&mut self, dir: i32) {
        let Some(id) = self.target_thread() else { return };
        let Some(g) = self.store.state.active_mut() else { return };
        let Some(i) = g.threads.iter().position(|t| t.id == id) else { return };
        // swap with the nearest non-archived neighbour in that direction
        let mut j = i as i32 + dir;
        while j >= 0 && (j as usize) < g.threads.len() && g.threads[j as usize].archived {
            j += dir;
        }
        if j < 0 || j as usize >= g.threads.len() {
            return;
        }
        g.threads.swap(i, j as usize);
        self.store.mark_dirty();
        if self.focus == Focus::Sidebar
            && let Some(p) = self.visible_threads().iter().position(|t| t.id == id) {
                self.sidebar_sel = p + 1;
            }
    }

    fn resize_split(&mut self, delta: f32) {
        if self.open_thread().is_none() || self.layout.narrow {
            return;
        }
        let r = &mut self.store.state.ui.split_ratio;
        *r = (*r + delta).clamp(0.2, 0.8);
        self.store.mark_dirty();
        self.sync_sizes();
    }

    fn toggle_sidebar(&mut self) {
        if self.layout.narrow {
            if self.overlay {
                self.overlay = false;
                if self.focus == Focus::Sidebar {
                    let f = if self.prev_focus == Focus::Sidebar { Focus::Main } else { self.prev_focus };
                    self.set_focus(f);
                }
            } else {
                self.overlay = true;
                self.set_focus(Focus::Sidebar);
            }
            return;
        }
        let v = &mut self.store.state.ui.sidebar_visible;
        *v = !*v;
        if !*v && self.focus == Focus::Sidebar {
            self.set_focus(Focus::Main);
        }
        self.store.mark_dirty();
        self.sync_sizes();
    }

    fn main_has_history(&self) -> bool {
        let Some(main_id) = self.store.state.active_main.as_deref() else { return false };
        let candidates = [
            self.main_transcript.clone(),
            Some(paths::transcript_path(&self.paths.project_dir, main_id)),
            paths::find_transcript(main_id),
        ];
        candidates.into_iter().flatten().any(|p| paths::transcript_has_messages(&p))
    }

    /// FR-10..FR-12: open the new-thread prompt.
    pub fn cmd_new_thread(&mut self) {
        if self.focus == Focus::Thread {
            self.set_status("Threads can't have threads — focus main (^]m) to fork it");
            return;
        }
        self.open_new_thread_prompt();
    }

    fn open_new_thread_prompt(&mut self) {
        if self.main_exited || self.main.pane.is_none() {
            self.set_status("Main is not running");
            return;
        }
        if !self.main_has_history() {
            self.set_status("Send a message in main first");
            return;
        }
        let dir = &self.paths.project_dir;
        let git_ok = git::repo_root(dir).is_some() && git::has_head(dir);
        let dirty = git_ok && git::is_dirty(dir).unwrap_or(false);
        self.modal = Some(Modal::NewThread { input: TextArea::single(""), worktree: false, git_ok, dirty, hint: None });
    }

    fn create_thread(&mut self, title: String, worktree: bool) -> Result<()> {
        let main_id = self.store.state.active_main.clone().context("main has no session yet")?;
        let id = format!("t_{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        let sid = uuid::Uuid::new_v4().to_string();
        let mut cwd = self.paths.project_dir.clone();
        let mut wt = None;
        if worktree {
            let repo = git::repo_root(&self.paths.project_dir).context("not a git repository")?;
            let (path, branch) =
                git::plan_worktree(&repo, &self.cfg.worktree.root, &self.cfg.worktree.branch_prefix, &text::slugify(&title));
            git::add_worktree(&repo, &path, &branch)?;
            let path = paths::canonical(&path);
            // Keep the same relative subfolder if cthread runs below the repo root.
            if let Ok(rel) = self.paths.project_dir.strip_prefix(paths::canonical(&repo)) {
                cwd = path.join(rel);
            } else {
                cwd = path.clone();
            }
            // Claude Code finds sessions by folder: copy main's transcript into
            // the worktree's project folder (Spike S3).
            let src = self
                .main_transcript
                .clone()
                .filter(|p| p.is_file())
                .or_else(|| paths::find_transcript(&main_id))
                .context("main's transcript was not found")?;
            let dst_dir = paths::transcript_dir(&cwd);
            std::fs::create_dir_all(&dst_dir)?;
            let dst = dst_dir.join(format!("{main_id}.jsonl"));
            std::fs::copy(&src, &dst).with_context(|| format!("copying transcript to {}", dst.display()))?;
            wt = Some(WorktreeRec { path, branch });
        }
        let rec = ThreadRec {
            id: id.clone(),
            title,
            session_id: None,
            pending_session_id: Some(sid.clone()),
            cwd: cwd.clone(),
            worktree: wt,
            created_at: now_utc(),
            last_viewed_at: Some(now_utc()),
            unread: false,
            archived: false,
            turns: 0,
        };
        let pty = self.compute_thread_pty();
        let args = vec!["--resume".into(), main_id, "--fork-session".into(), "--session-id".into(), sid];
        let pane = self.spawn(Owner::Thread(id.clone()), cwd, args, pty.height, pty.width)?;
        let g = self.store.state.active_mut().context("no active generation")?;
        g.threads.push(rec);
        let mut s = Session::new();
        s.pane = Some(pane);
        s.placeholder = "Forking main…";
        self.threads.insert(id.clone(), s);
        self.store.mark_dirty();
        self.open(&id);
        Ok(())
    }

    fn cmd_rename(&mut self) {
        let Some(id) = self.target_thread() else {
            self.set_status("No thread selected");
            return;
        };
        let title = self.store.state.thread(&id).map(|t| t.title.clone()).unwrap_or_default();
        self.modal = Some(Modal::Rename { id, input: TextArea::single(&title), hint: None });
    }

    fn cmd_archive(&mut self) {
        let Some(id) = self.target_thread() else {
            self.set_status("No thread selected");
            return;
        };
        self.discard(&id);
        if let Some(t) = self.store.state.thread_mut(&id) {
            t.archived = true;
            let title = t.title.clone();
            self.set_status(format!("Archived \"{title}\" — ^]A to restore"));
        }
        if self.open_thread() == Some(id.as_str()) {
            self.close_thread();
        }
        let n = self.visible_threads().len();
        self.sidebar_sel = self.sidebar_sel.min(n);
        self.store.mark_dirty();
    }

    fn cmd_delete(&mut self) {
        let Some(id) = self.target_thread() else {
            self.set_status("No thread selected");
            return;
        };
        self.modal = Some(Modal::ConfirmDelete { id });
    }

    /// FR-18 / FR-52. `remove_worktree`: None = keep, Some(force).
    fn delete_thread(&mut self, id: &str, remove_worktree: Option<bool>) {
        self.discard(id);
        self.threads.remove(id);
        if self.open_thread() == Some(id) {
            self.close_thread();
        }
        let Some(g) = self.store.state.active_mut() else { return };
        let Some(i) = g.threads.iter().position(|t| t.id == id) else { return };
        let rec = g.threads.remove(i);
        self.store.mark_dirty();
        if let (Some(force), Some(wt)) = (remove_worktree, &rec.worktree) {
            let repo = git::repo_root(&self.paths.project_dir).unwrap_or(self.paths.project_dir.clone());
            // The process may still be shutting down; give it a moment.
            std::thread::sleep(Duration::from_millis(100));
            match git::remove_worktree(&repo, &wt.path, &wt.branch, force) {
                Ok(()) => self.set_status(format!("Deleted \"{}\" and removed its worktree", rec.title)),
                Err(e) => self.set_status(format!("Deleted thread; worktree not removed: {e:#}")),
            }
        } else {
            self.set_status(format!("Deleted \"{}\"", rec.title));
        }
        let n = self.visible_threads().len();
        self.sidebar_sel = self.sidebar_sel.min(n);
    }

    fn cmd_quit(&mut self) {
        let working = self.main.activity == Activity::Working
            || self.threads.values().any(|s| s.live() && s.activity == Activity::Working);
        if working {
            self.modal = Some(Modal::ConfirmQuit);
        } else {
            self.quit = true;
        }
    }

    /// FR-40, FR-42: start a pull for the open (or selected) thread.
    pub fn cmd_pull(&mut self) {
        let Some(id) = self.target_thread() else {
            self.set_status("Open or select a thread to pull");
            return;
        };
        let Some(t) = self.store.state.thread(&id).cloned() else { return };
        if t.turns == 0 {
            self.set_status("Nothing new in this thread");
            return;
        }
        let Some(sid) = t.resume_id().map(str::to_string) else {
            self.set_status("Thread has no session yet");
            return;
        };
        let job = self.next_job;
        self.next_job += 1;
        let cancel = self.start_summary(job, sid, t.cwd.clone());
        self.modal = Some(Modal::Pull {
            thread: id,
            title: t.title.clone(),
            job,
            cancel,
            phase: PullPhase::Loading,
            text: TextArea::multi(""),
            editing: false,
        });
    }

    fn start_summary(&self, job: u64, session_id: String, cwd: PathBuf) -> Arc<AtomicBool> {
        let n = self.notify.clone();
        summarizer::start(
            SummaryRequest {
                job,
                command: self.cfg.claude.pull_argv(),
                session_id,
                cwd,
                prompt: self.cfg.pull.prompt.clone(),
                timeout: Duration::from_secs(self.cfg.pull.timeout_seconds),
            },
            Arc::new(move |r| n(AppEvent::Summary(r))),
        )
    }

    fn regenerate(&mut self) {
        let Some(Modal::Pull { thread, .. }) = &self.modal else { return };
        let Some(t) = self.store.state.thread(thread).cloned() else { return };
        let Some(sid) = t.resume_id().map(str::to_string) else { return };
        let job = self.next_job;
        self.next_job += 1;
        let new_cancel = self.start_summary(job, sid, t.cwd);
        if let Some(Modal::Pull { job: j, cancel, phase, text, editing, .. }) = &mut self.modal {
            cancel.store(true, Ordering::Relaxed);
            *cancel = new_cancel;
            *j = job;
            *phase = PullPhase::Loading;
            text.set("");
            *editing = false;
        }
    }

    /// FR-41: paste into main's input with bracketed paste; never Enter.
    fn insert_pull(&mut self, title: &str, body: &str) -> bool {
        let Some(p) = self.main.pane.as_ref().filter(|_| !self.main_exited) else {
            self.set_status("Main is not running");
            return false;
        };
        let text = format!("[Context from thread \"{title}\"]\n{}", body.trim_end());
        p.input(&keys::bracketed(&text));
        true
    }

    /// Result of `$EDITOR` (None = editor failed / cancelled).
    pub fn editor_done(&mut self, result: Result<String, String>) {
        match result {
            Ok(s) => {
                if let Some(Modal::Pull { text, phase, .. }) = &mut self.modal {
                    text.set(s.trim_end());
                    *phase = PullPhase::Ready;
                }
            }
            Err(e) => self.set_status(format!("Editor: {e}")),
        }
        self.dirty = true;
    }

    // ---------------------------------------------------------------- modals

    fn on_modal_key(&mut self, k: KeyEvent) {
        let Some(modal) = self.modal.take() else { return };
        let esc = k.code == KeyCode::Esc;
        let ch = |c: char| k.code == KeyCode::Char(c) && !k.modifiers.contains(KeyModifiers::CONTROL);
        self.modal = match modal {
            Modal::Help => {
                if esc || ch('q') || ch('?') || k.code == KeyCode::Enter { None } else { Some(Modal::Help) }
            }
            Modal::ConfirmQuit => {
                if ch('y') || ch('Y') {
                    self.quit = true;
                    None
                } else if esc || ch('n') || ch('N') {
                    None
                } else {
                    Some(Modal::ConfirmQuit)
                }
            }
            Modal::NewThread { mut input, mut worktree, git_ok, dirty, mut hint } => {
                if esc {
                    None
                } else if k.code == KeyCode::Tab || k.code == KeyCode::BackTab {
                    if git_ok {
                        worktree = !worktree;
                    }
                    Some(Modal::NewThread { input, worktree, git_ok, dirty, hint })
                } else if k.code == KeyCode::Enter {
                    match text::validate_title(&input.text) {
                        Err(e) => Some(Modal::NewThread { input, worktree, git_ok, dirty, hint: Some(e.into()) }),
                        Ok(title) => {
                            if let Err(e) = self.create_thread(title, worktree) {
                                self.set_status(format!("Could not create thread: {e:#}"));
                            }
                            None
                        }
                    }
                } else {
                    input.handle(&k);
                    hint = None;
                    Some(Modal::NewThread { input, worktree, git_ok, dirty, hint })
                }
            }
            Modal::Rename { id, mut input, hint: _ } => {
                if esc {
                    None
                } else if k.code == KeyCode::Enter {
                    match text::validate_title(&input.text) {
                        Err(e) => Some(Modal::Rename { id, input, hint: Some(e.into()) }),
                        Ok(title) => {
                            if let Some(t) = self.store.state.thread_mut(&id) {
                                t.title = title;
                                self.store.mark_dirty();
                            }
                            None
                        }
                    }
                } else {
                    input.handle(&k);
                    Some(Modal::Rename { id, input, hint: None })
                }
            }
            Modal::Archived { mut selected } => {
                let list: Vec<String> = self.archived_threads().iter().map(|t| t.id.clone()).collect();
                if esc || ch('q') {
                    None
                } else if list.is_empty() {
                    Some(Modal::Archived { selected: 0 })
                } else if ch('j') || k.code == KeyCode::Down {
                    selected = (selected + 1).min(list.len() - 1);
                    Some(Modal::Archived { selected })
                } else if ch('k') || k.code == KeyCode::Up {
                    selected = selected.saturating_sub(1);
                    Some(Modal::Archived { selected })
                } else if k.code == KeyCode::Enter || ch('r') {
                    let id = &list[selected.min(list.len() - 1)];
                    if let Some(t) = self.store.state.thread_mut(id) {
                        t.archived = false;
                        let title = t.title.clone();
                        self.store.mark_dirty();
                        self.set_status(format!("Restored \"{title}\""));
                    }
                    let left = list.len() - 1;
                    if left == 0 { None } else { Some(Modal::Archived { selected: selected.min(left - 1) }) }
                } else if ch('D') {
                    Some(Modal::ConfirmDelete { id: list[selected.min(list.len() - 1)].clone() })
                } else {
                    Some(Modal::Archived { selected })
                }
            }
            Modal::ConfirmDelete { id } => {
                if ch('y') || ch('Y') {
                    let has_wt = self.store.state.thread(&id).is_some_and(|t| t.worktree.is_some());
                    if has_wt {
                        Some(Modal::DeleteWorktree { id, confirm_dirty: false })
                    } else {
                        self.delete_thread(&id, None);
                        None
                    }
                } else if esc || ch('n') || ch('N') {
                    None
                } else {
                    Some(Modal::ConfirmDelete { id })
                }
            }
            Modal::DeleteWorktree { id, confirm_dirty } => {
                if esc || (confirm_dirty && (ch('n') || ch('N'))) {
                    None
                } else if !confirm_dirty && ch('k') {
                    self.delete_thread(&id, None);
                    None
                } else if !confirm_dirty && ch('r') {
                    let wt_dirty = self
                        .store
                        .state
                        .thread(&id)
                        .and_then(|t| t.worktree.as_ref())
                        .map(|w| w.path.exists() && git::is_dirty(&w.path).unwrap_or(true))
                        .unwrap_or(false);
                    if wt_dirty {
                        Some(Modal::DeleteWorktree { id, confirm_dirty: true })
                    } else {
                        self.delete_thread(&id, Some(false));
                        None
                    }
                } else if confirm_dirty && (ch('y') || ch('Y')) {
                    self.delete_thread(&id, Some(true));
                    None
                } else {
                    Some(Modal::DeleteWorktree { id, confirm_dirty })
                }
            }
            Modal::Pull { thread, title, job, cancel, phase, mut text, editing } => {
                if editing {
                    if esc {
                        Some(Modal::Pull { thread, title, job, cancel, phase, text, editing: false })
                    } else {
                        text.handle(&k);
                        Some(Modal::Pull { thread, title, job, cancel, phase, text, editing })
                    }
                } else if esc {
                    cancel.store(true, Ordering::Relaxed);
                    None
                } else if ch('r') && phase != PullPhase::Loading {
                    self.modal = Some(Modal::Pull { thread, title, job, cancel, phase, text, editing });
                    self.regenerate();
                    return;
                } else if phase == PullPhase::Ready && k.code == KeyCode::Enter {
                    if self.insert_pull(&title, &text.text) {
                        self.focus_main();
                        None
                    } else {
                        Some(Modal::Pull { thread, title, job, cancel, phase, text, editing })
                    }
                } else if phase == PullPhase::Ready && ch('e') {
                    Some(Modal::Pull { thread, title, job, cancel, phase, text, editing: true })
                } else if phase == PullPhase::Ready && ch('E') {
                    self.editor_request = Some(EditorRequest { text: text.text.clone() });
                    Some(Modal::Pull { thread, title, job, cancel, phase, text, editing })
                } else {
                    Some(Modal::Pull { thread, title, job, cancel, phase, text, editing })
                }
            }
        };
    }

    // ----------------------------------------------------------------- mouse

    fn on_mouse(&mut self, m: MouseEvent) {
        if !self.cfg.ui.mouse || self.modal.is_some() || self.layout.too_small {
            return;
        }
        let (x, y) = (m.column, m.row);
        let inside = |r: Option<Rect>| r.is_some_and(|r| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(dx) = self.layout.divider_x
                    && (x == dx || x == dx + 1) && inside(self.layout.main.or(self.layout.thread)) {
                        self.dragging = true;
                        return;
                    }
                let side = self.layout.overlay.or(self.layout.sidebar);
                if inside(side) {
                    self.click_sidebar(side.unwrap(), y);
                } else if inside(self.layout.thread) {
                    let t = self.layout.thread.unwrap();
                    if y == t.y && x + 3 >= t.x + t.width && x < t.x + t.width {
                        self.close_thread();
                    } else {
                        self.set_focus(Focus::Thread);
                    }
                } else if inside(self.layout.main) {
                    self.set_focus(Focus::Main);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.dragging => {
                let p = self.layout.panes_area;
                if p.width > 0 {
                    let ratio = (x.saturating_sub(p.x) as f32 + 1.0) / p.width as f32;
                    self.store.state.ui.split_ratio = ratio.clamp(0.2, 0.8);
                    self.store.mark_dirty();
                    self.sync_sizes();
                }
            }
            MouseEventKind::Up(MouseButton::Left) => self.dragging = false,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = m.kind == MouseEventKind::ScrollUp;
                let (pane, area) = if inside(self.layout.thread) {
                    (self.open_thread().and_then(|id| self.threads.get(id)).and_then(|s| s.pane.as_ref()), self.layout.thread_pty)
                } else if inside(self.layout.main) {
                    (self.main.pane.as_ref(), self.layout.main_pty)
                } else {
                    return;
                };
                let Some(p) = pane else { return };
                let (mode, enc) = {
                    let parser = p.parser();
                    (parser.screen().mouse_protocol_mode(), parser.screen().mouse_protocol_encoding())
                };
                if mode != vt100::MouseProtocolMode::None {
                    let col = x.saturating_sub(area.x);
                    let row = y.saturating_sub(area.y);
                    p.write(&keys::encode_wheel(up, col, row, enc == vt100::MouseProtocolEncoding::Sgr));
                } else {
                    p.scroll(if up { 3 } else { -3 });
                }
            }
            _ => {}
        }
    }

    fn click_sidebar(&mut self, side: Rect, y: u16) {
        // Row layout (see ui::draw_sidebar): border, main, divider, threads…, footer.
        let row = y.saturating_sub(side.y);
        let collapsed = self.layout.narrow && self.layout.overlay.is_none();
        let footer = side.height.saturating_sub(2);
        if row == 1 {
            self.focus_main();
            return;
        }
        if !collapsed && row == footer {
            // "+ New thread" always forks main (FR-12).
            self.open_new_thread_prompt();
            return;
        }
        if row >= 3 {
            let i = (row - 3) as usize;
            if let Some(id) = self.visible_threads().get(i).map(|t| t.id.clone()) {
                self.open(&id);
            }
        }
    }
}
