//! Rendered-screen snapshots (insta) of the layouts and modals. No sessions
//! are spawned; panes are empty so the chrome is deterministic.

use cthread::app::{App, AppOptions, Focus, Modal, PullPhase};
use cthread::config::Config;
use cthread::paths::ProjectPaths;
use cthread::state::{State, Store, ThreadRec, WorktreeRec};
use cthread::textarea::TextArea;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

fn rec(id: &str, title: &str) -> ThreadRec {
    ThreadRec {
        id: id.into(),
        title: title.into(),
        session_id: Some(format!("s-{id}")),
        pending_session_id: None,
        cwd: "/p".into(),
        worktree: None,
        created_at: "2026-11-20T07:32:10Z".parse().unwrap(),
        last_viewed_at: None,
        unread: false,
        archived: false,
        turns: 2,
    }
}

fn app(tmp: &tempfile::TempDir) -> App {
    unsafe { std::env::set_var("TZ", "UTC") };
    let project = tmp.path().join("shop");
    std::fs::create_dir_all(&project).unwrap();
    let paths = ProjectPaths::with_state_dir(&project, tmp.path().join("state"));
    let mut st = State::new(&paths.project_dir);
    st.start_generation("main-1", "2026-11-20T07:30:00Z".parse().unwrap());
    let g = st.active_mut().unwrap();
    g.threads.push(rec("t1", "stripe vs paddle pricing"));
    let mut r = rec("t2", "auth refactor with a very long title that will not fit");
    r.unread = true;
    g.threads.push(r);
    g.threads.push(rec("t3", "fix flaky test"));
    let mut r = rec("t4", "db schema");
    r.worktree = Some(WorktreeRec { path: "/p-db-schema".into(), branch: "cthread/db-schema".into() });
    g.threads.push(r);
    let mut r = rec("t5", "old idea");
    r.archived = true;
    g.threads.push(r);
    let mut r = rec("t6", "日本語のスレッドタイトル");
    r.unread = false;
    g.threads.push(r);
    let store = Store::new(paths.state_file.clone(), st, true);
    
    App::new(Config::default(), paths, store, AppOptions { hook_exe: "/bin/true".into() }, Arc::new(|_| {})).unwrap()
}

fn render(a: &mut App, w: u16, h: u16) -> String {
    a.size = (w, h);
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| cthread::ui::draw(f, a)).unwrap();
    let buf = term.backend().buffer().clone();
    let mut s = String::new();
    for y in 0..h {
        let mut line = String::new();
        let mut skip = 0;
        for x in 0..w {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let sym = buf[(x, y)].symbol();
            skip = unicode_width::UnicodeWidthStr::width(sym).saturating_sub(1);
            line.push_str(sym);
        }
        s.push_str(line.trim_end());
        s.push('\n');
    }
    s
}

#[test]
fn wide_thread_open() {
    let tmp = tempfile::tempdir().unwrap();
    let mut a = app(&tmp);
    a.store.state.active_mut().unwrap().open_thread = Some("t1".into());
    a.focus = Focus::Thread;
    insta::assert_snapshot!(render(&mut a, 130, 16));
}

#[test]
fn wide_no_thread() {
    let tmp = tempfile::tempdir().unwrap();
    let mut a = app(&tmp);
    insta::assert_snapshot!(render(&mut a, 130, 16));
}

#[test]
fn wide_worktree_thread_header() {
    let tmp = tempfile::tempdir().unwrap();
    let mut a = app(&tmp);
    a.store.state.active_mut().unwrap().open_thread = Some("t4".into());
    a.focus = Focus::Main;
    insta::assert_snapshot!(render(&mut a, 130, 16));
}

#[test]
fn narrow_thread_and_overlay() {
    let tmp = tempfile::tempdir().unwrap();
    let mut a = app(&tmp);
    a.store.state.active_mut().unwrap().open_thread = Some("t1".into());
    a.focus = Focus::Thread;
    let collapsed = render(&mut a, 90, 16);
    insta::assert_snapshot!("narrow_collapsed", collapsed);
    a.overlay = true;
    a.focus = Focus::Sidebar;
    a.sidebar_sel = 2;
    insta::assert_snapshot!("narrow_overlay", render(&mut a, 90, 16));
}

#[test]
fn new_thread_modal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut a = app(&tmp);
    a.modal = Some(Modal::NewThread { input: TextArea::single("stripe vs paddle pricing"), worktree: false, git_ok: true, dirty: true, hint: None });
    insta::assert_snapshot!(render(&mut a, 130, 18));
    a.modal = Some(Modal::NewThread { input: TextArea::single(""), worktree: false, git_ok: false, dirty: false, hint: Some("Title is required".into()) });
    insta::assert_snapshot!("new_thread_modal_empty", render(&mut a, 130, 18));
}

#[test]
fn pull_modal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut a = app(&tmp);
    a.store.state.active_mut().unwrap().open_thread = Some("t1".into());
    a.modal = Some(Modal::Pull {
        thread: "t1".into(),
        title: "stripe vs paddle pricing".into(),
        job: 1,
        cancel: Arc::new(AtomicBool::new(false)),
        phase: PullPhase::Ready,
        text: TextArea::multi(
            "- Paddle is merchant of record, handles EU VAT\n- Stripe is ~1% cheaper but we'd file VAT ourselves\n- Open question: do we need invoicing for B2B?",
        ),
        editing: false,
    });
    insta::assert_snapshot!(render(&mut a, 130, 20));
}

#[test]
fn help_overlay() {
    let tmp = tempfile::tempdir().unwrap();
    let mut a = app(&tmp);
    a.modal = Some(Modal::Help);
    insta::assert_snapshot!(render(&mut a, 130, 28));
}

#[test]
fn archived_and_delete_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let mut a = app(&tmp);
    a.modal = Some(Modal::Archived { selected: 0 });
    insta::assert_snapshot!("archived", render(&mut a, 130, 16));
    a.modal = Some(Modal::DeleteWorktree { id: "t4".into(), confirm_dirty: false });
    insta::assert_snapshot!("delete_worktree", render(&mut a, 130, 16));
}

#[test]
fn too_small() {
    let tmp = tempfile::tempdir().unwrap();
    let mut a = app(&tmp);
    insta::assert_snapshot!(render(&mut a, 50, 12));
}
