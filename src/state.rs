//! Persistent state: one JSON file per project folder, grouped by main
//! session ID ("generations").

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const VERSION: u32 = 1;
pub const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct State {
    pub version: u32,
    pub project_dir: PathBuf,
    pub active_main: Option<String>,
    #[serde(default)]
    pub generations: BTreeMap<String, Generation>,
    #[serde(default)]
    pub ui: UiState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Generation {
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub threads: Vec<ThreadRec>,
    #[serde(default)]
    pub open_thread: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThreadRec {
    pub id: String,
    pub title: String,
    /// Confirmed by a SessionStart hook.
    pub session_id: Option<String>,
    /// Pre-assigned with --session-id but not yet confirmed by a hook.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_session_id: Option<String>,
    pub cwd: PathBuf,
    pub worktree: Option<WorktreeRec>,
    pub created_at: DateTime<Utc>,
    pub last_viewed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub unread: bool,
    #[serde(default)]
    pub archived: bool,
    /// Started with read-only tools. Claude Code doesn't restore tool limits
    /// on resume, so cthread passes them on every start.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub read_only: bool,
    /// Prompts submitted in this thread since the fork (for "Nothing new").
    #[serde(default)]
    pub turns: u32,
}

impl ThreadRec {
    /// The session ID to resume: confirmed, else pre-assigned.
    pub fn resume_id(&self) -> Option<&str> {
        self.session_id.as_deref().or(self.pending_session_id.as_deref())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorktreeRec {
    pub path: PathBuf,
    pub branch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UiState {
    pub split_ratio: f32,
    pub sidebar_visible: bool,
}

impl Default for UiState {
    fn default() -> Self {
        Self { split_ratio: 0.5, sidebar_visible: true }
    }
}

/// What happened to generations after a SessionStart in main.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenChange {
    None,
    /// A new, empty generation became active (first start, /clear).
    New { old: Option<String> },
    /// /resume switched to another (possibly new) generation.
    Switched { old: String },
    /// The active generation's key was corrected to the real session ID.
    Renamed,
}

impl State {
    pub fn new(project_dir: &Path) -> State {
        State {
            version: VERSION,
            project_dir: project_dir.to_path_buf(),
            active_main: None,
            generations: BTreeMap::new(),
            ui: UiState::default(),
        }
    }

    pub fn active(&self) -> Option<&Generation> {
        self.active_main.as_ref().and_then(|id| self.generations.get(id))
    }

    pub fn active_mut(&mut self) -> Option<&mut Generation> {
        let id = self.active_main.clone()?;
        self.generations.get_mut(&id)
    }

    pub fn threads(&self) -> &[ThreadRec] {
        self.active().map(|g| g.threads.as_slice()).unwrap_or(&[])
    }

    pub fn thread(&self, id: &str) -> Option<&ThreadRec> {
        self.active()?.threads.iter().find(|t| t.id == id)
    }

    pub fn thread_mut(&mut self, id: &str) -> Option<&mut ThreadRec> {
        self.active_mut()?.threads.iter_mut().find(|t| t.id == id)
    }

    /// Start a brand-new main generation (first start in a folder).
    pub fn start_generation(&mut self, main_id: &str, now: DateTime<Utc>) {
        self.generations
            .entry(main_id.to_string())
            .or_insert_with(|| Generation { created_at: now, threads: vec![], open_thread: None });
        self.active_main = Some(main_id.to_string());
    }

    /// Apply a SessionStart from main's pane (FR-60, FR-61, /compact).
    /// `first_after_spawn` is true for the first SessionStart after cthread
    /// launched main: a mismatching ID then means the pre-assigned/resumed ID
    /// was not what Claude Code used, so the generation is re-keyed instead of
    /// losing its threads.
    pub fn on_main_session_start(
        &mut self,
        new_id: &str,
        source: &str,
        first_after_spawn: bool,
        now: DateTime<Utc>,
    ) -> GenChange {
        let Some(old) = self.active_main.clone() else {
            self.start_generation(new_id, now);
            return GenChange::New { old: None };
        };
        if old == new_id {
            return GenChange::None;
        }
        if source == "clear" {
            self.start_generation(new_id, now);
            return GenChange::New { old: Some(old) };
        }
        if first_after_spawn && !self.generations.contains_key(new_id) {
            // Re-key: same conversation, different ID than expected.
            if let Some(g) = self.generations.remove(&old) {
                self.generations.insert(new_id.to_string(), g);
            }
            self.active_main = Some(new_id.to_string());
            return GenChange::Renamed;
        }
        // /resume (or anything else) into another session.
        self.start_generation(new_id, now);
        GenChange::Switched { old }
    }

    /// Remove inactive generations created before `cutoff`. Returns how many.
    pub fn prune(&mut self, cutoff: DateTime<Utc>) -> usize {
        let active = self.active_main.clone();
        let before = self.generations.len();
        self.generations.retain(|id, g| Some(id) == active.as_ref() || g.created_at >= cutoff);
        before - self.generations.len()
    }
}

/// Parse "30d", "12h", "90m".
pub fn parse_duration(s: &str) -> Option<chrono::Duration> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit())?);
    let n: i64 = num.parse().ok()?;
    match unit {
        "d" => Some(chrono::Duration::days(n)),
        "h" => Some(chrono::Duration::hours(n)),
        "m" => Some(chrono::Duration::minutes(n)),
        "w" => Some(chrono::Duration::weeks(n)),
        _ => None,
    }
}

pub enum Loaded {
    Missing,
    Ok(State),
    /// The file exists but cthread must not touch it (unknown version or
    /// unreadable). Carries the warning.
    Untouchable(String),
}

pub fn load(path: &Path) -> Loaded {
    let s = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Loaded::Missing,
        Err(e) => return Loaded::Untouchable(format!("cannot read state file: {e}")),
    };
    let v: serde_json::Value = match serde_json::from_str(&s) {
        Ok(v) => v,
        Err(e) => return Loaded::Untouchable(format!("state file is not valid JSON ({e}); not touching it")),
    };
    let ver = v.get("version").and_then(|v| v.as_u64());
    if ver != Some(VERSION as u64) {
        return Loaded::Untouchable(format!(
            "state file has unknown version {:?}; running without saving",
            ver
        ));
    }
    match serde_json::from_value::<State>(v) {
        Ok(st) => Loaded::Ok(st),
        Err(e) => Loaded::Untouchable(format!("state file could not be parsed ({e}); running without saving")),
    }
}

/// Atomic write: temp file in the same folder, fsync, rename.
pub fn save(path: &Path, state: &State) -> Result<()> {
    let dir = path.parent().context("state path has no parent")?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.tmp{}",
        path.file_name().unwrap().to_string_lossy(),
        std::process::id()
    ));
    {
        let mut f = File::create(&tmp)?;
        f.write_all(serde_json::to_string_pretty(state)?.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// State plus debounced, atomic persistence.
pub struct Store {
    pub path: PathBuf,
    pub state: State,
    pub readonly: bool,
    dirty_since: Option<Instant>,
}

impl Store {
    pub fn new(path: PathBuf, state: State, readonly: bool) -> Store {
        Store { path, state, readonly, dirty_since: None }
    }

    pub fn mark_dirty(&mut self) {
        if self.dirty_since.is_none() {
            self.dirty_since = Some(Instant::now());
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty_since.is_some()
    }

    /// Save if dirty for at least the debounce interval.
    pub fn maybe_save(&mut self, now: Instant) -> Result<()> {
        match self.dirty_since {
            Some(t) if now.duration_since(t) >= SAVE_DEBOUNCE => self.save_now(),
            _ => Ok(()),
        }
    }

    pub fn save_now(&mut self) -> Result<()> {
        self.dirty_since = None;
        if self.readonly {
            return Ok(());
        }
        save(&self.path, &self.state)
    }
}

/// Single-instance lock (flock on a file next to the state file). Released
/// when dropped / when the process exits.
pub struct InstanceLock {
    _file: File,
}

pub fn acquire_lock(path: &Path) -> Result<InstanceLock> {
    use std::os::unix::io::AsRawFd;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(path)?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        bail!("already running");
    }
    let mut f = &file;
    let _ = f.write_all(format!("{}\n", std::process::id()).as_bytes());
    Ok(InstanceLock { _file: file })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        "2026-11-24T07:30:00Z".parse().unwrap()
    }

    fn thread(id: &str) -> ThreadRec {
        ThreadRec {
            id: id.into(),
            title: id.into(),
            session_id: Some(format!("s-{id}")),
            pending_session_id: None,
            cwd: "/p".into(),
            worktree: None,
            created_at: now(),
            last_viewed_at: None,
            unread: false,
            archived: false,
            read_only: false,
            turns: 0,
        }
    }

    #[test]
    fn roundtrip_and_atomic_save() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.json");
        let mut s = State::new(Path::new("/p"));
        s.start_generation("m1", now());
        s.active_mut().unwrap().threads.push(thread("a"));
        save(&p, &s).unwrap();
        let Loaded::Ok(back) = load(&p) else { panic!() };
        assert_eq!(back, s);
        // no temp files left behind
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }

    #[test]
    fn prd_example_parses() {
        let j = r#"{
          "version": 1, "project_dir": "/Users/me/code/shop", "active_main": "5f0c",
          "generations": { "5f0c": { "created_at": "2026-11-24T07:30:00Z", "threads": [
            { "id": "t_8a1", "title": "stripe vs paddle pricing", "session_id": "c91e",
              "cwd": "/Users/me/code/shop", "worktree": null, "created_at": "2026-11-24T07:32:10Z",
              "last_viewed_at": "2026-11-24T08:01:00Z", "unread": true, "archived": false },
            { "id": "t_b77", "title": "db schema", "session_id": "0d42", "cwd": "/Users/me/code/shop-db-schema",
              "worktree": { "path": "/Users/me/code/shop-db-schema", "branch": "cthread/db-schema" },
              "created_at": "2026-11-24T07:40:00Z", "last_viewed_at": null, "unread": false, "archived": false }
          ], "open_thread": "t_8a1" } },
          "ui": { "split_ratio": 0.5, "sidebar_visible": true } }"#;
        let s: State = serde_json::from_str(j).unwrap();
        assert_eq!(s.threads().len(), 2);
        assert_eq!(s.threads()[1].worktree.as_ref().unwrap().branch, "cthread/db-schema");
    }

    #[test]
    fn unknown_version_is_untouchable() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.json");
        std::fs::write(&p, r#"{"version": 99}"#).unwrap();
        assert!(matches!(load(&p), Loaded::Untouchable(_)));
        std::fs::write(&p, "garbage").unwrap();
        assert!(matches!(load(&p), Loaded::Untouchable(_)));
        assert!(matches!(load(&d.path().join("nope.json")), Loaded::Missing));
        // readonly store never writes
        let mut st = Store::new(p.clone(), State::new(Path::new("/p")), true);
        st.mark_dirty();
        st.save_now().unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "garbage");
    }

    #[test]
    fn debounce() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.json");
        let mut st = Store::new(p.clone(), State::new(Path::new("/p")), false);
        st.mark_dirty();
        let t0 = Instant::now();
        st.maybe_save(t0).unwrap();
        assert!(!p.exists());
        st.maybe_save(t0 + SAVE_DEBOUNCE + Duration::from_millis(1)).unwrap();
        assert!(p.exists());
        assert!(!st.is_dirty());
    }

    #[test]
    fn generation_transitions() {
        let mut s = State::new(Path::new("/p"));
        // first start
        assert_eq!(s.on_main_session_start("m1", "startup", true, now()), GenChange::New { old: None });
        s.active_mut().unwrap().threads.push(thread("a"));
        // relaunch with same id, compact: no change
        assert_eq!(s.on_main_session_start("m1", "resume", true, now()), GenChange::None);
        assert_eq!(s.on_main_session_start("m1", "compact", false, now()), GenChange::None);
        // /clear: new empty generation, old one kept
        assert_eq!(
            s.on_main_session_start("m2", "clear", false, now()),
            GenChange::New { old: Some("m1".into()) }
        );
        assert!(s.threads().is_empty());
        assert_eq!(s.generations.len(), 2);
        // /resume back to m1 brings its threads back
        assert_eq!(
            s.on_main_session_start("m1", "resume", false, now()),
            GenChange::Switched { old: "m2".into() }
        );
        assert_eq!(s.threads().len(), 1);
        // /resume to unknown id: new empty generation
        assert_eq!(
            s.on_main_session_start("m3", "resume", false, now()),
            GenChange::Switched { old: "m1".into() }
        );
        assert!(s.threads().is_empty());
    }

    #[test]
    fn first_start_id_mismatch_rekeys() {
        let mut s = State::new(Path::new("/p"));
        s.start_generation("pre", now());
        s.active_mut().unwrap().threads.push(thread("a"));
        assert_eq!(s.on_main_session_start("real", "startup", true, now()), GenChange::Renamed);
        assert_eq!(s.active_main.as_deref(), Some("real"));
        assert_eq!(s.threads().len(), 1);
        assert!(!s.generations.contains_key("pre"));
    }

    #[test]
    fn prune_keeps_active() {
        let mut s = State::new(Path::new("/p"));
        s.start_generation("old", now() - chrono::Duration::days(40));
        s.start_generation("new", now() - chrono::Duration::days(1));
        s.start_generation("act", now() - chrono::Duration::days(100));
        assert_eq!(s.prune(now() - parse_duration("30d").unwrap()), 1);
        assert!(s.generations.contains_key("act"));
        assert!(s.generations.contains_key("new"));
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("30d"), Some(chrono::Duration::days(30)));
        assert_eq!(parse_duration("12h"), Some(chrono::Duration::hours(12)));
        assert_eq!(parse_duration("x"), None);
        assert_eq!(parse_duration("10y"), None);
    }

    #[test]
    fn lock_is_exclusive() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.lock");
        let l = acquire_lock(&p).unwrap();
        // flock is per open file description, so a second open in the same
        // process conflicts too.
        assert!(acquire_lock(&p).is_err());
        drop(l);
        // flock locks are shared with forked children. Tests that spawn
        // processes run in parallel, and a child forked while we held the
        // lock keeps it until it execs, so allow a moment for that.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while acquire_lock(&p).is_err() {
            assert!(std::time::Instant::now() < deadline, "lock not released after drop");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}
