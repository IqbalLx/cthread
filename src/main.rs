use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use cthread::app::{App, AppEvent, AppOptions};
use cthread::config::{self, Config};
use cthread::paths::{self, ProjectPaths};
use cthread::state::{self, Loaded, State, Store};
use cthread::{git, hooks, layout, ui};
use crossterm::event::{self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::{Write, stdout};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(name = "cthread", version, about = "Slack-style threads for Claude Code")]
struct Cli {
    /// Command that starts Claude Code, e.g. "ccr code" or a wrapper script.
    /// Overrides [claude] command in the config file.
    #[arg(long, global = true, value_name = "COMMAND")]
    claude: Option<String>,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print claude, git, state and hook relay diagnostics.
    Doctor,
    /// Internal: relay a Claude Code hook event to the running cthread.
    #[command(hide = true)]
    Hook,
    /// Remove old main generations from state (never transcripts or worktrees).
    Prune {
        /// e.g. 30d, 12h
        #[arg(long, default_value = "30d")]
        older_than: String,
    },
}

fn main() {
    let cli = Cli::parse();
    let r = match cli.cmd {
        Some(Cmd::Hook) => {
            hooks::run_hook_client();
            Ok(())
        }
        Some(Cmd::Doctor) => doctor(cli.claude),
        Some(Cmd::Prune { older_than }) => prune(&older_than),
        None => run(cli.claude),
    };
    if let Err(e) = r {
        eprintln!("cthread: {e:#}");
        std::process::exit(1);
    }
}

fn project_dir() -> Result<PathBuf> {
    Ok(paths::canonical(&std::env::current_dir().context("no current folder")?))
}

/// Load the config and apply the `--claude` override.
fn load_config(claude: Option<String>) -> Result<(Config, Vec<String>)> {
    let (mut cfg, warnings) = Config::load();
    if let Some(c) = claude {
        let line = config::CommandLine::Line(c);
        line.argv().map_err(|e| anyhow::anyhow!("--claude: {e}"))?;
        cfg.claude.command = Some(line);
    }
    Ok((cfg, warnings))
}

/// Resolve argv[0] on PATH. Shell aliases and functions are not visible to
/// cthread; use a wrapper script for those.
fn resolve(argv: &[String], key: &str) -> Result<Vec<String>> {
    let prog = &argv[0];
    let found = paths::which(prog).with_context(|| {
        format!(
            "{prog:?} not found ({key}). Install Claude Code, or set [claude] {key} in {} or pass --claude. \
             Shell aliases/functions aren't visible to cthread: point it at a script instead.",
            config::config_path().map(|p| p.display().to_string()).unwrap_or_default()
        )
    })?;
    let mut v = argv.to_vec();
    v[0] = found.to_string_lossy().to_string();
    Ok(v)
}

fn show_cmd(argv: &[String]) -> String {
    shell_words::join(argv)
}

fn doctor(claude: Option<String>) -> Result<()> {
    // Behave like a normal CLI when piped into `head` etc.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    let (cfg, warnings) = load_config(claude)?;
    for w in &warnings {
        println!("warning: {w}");
    }
    let dir = project_dir()?;
    let p = ProjectPaths::new(&dir);
    let argv = cfg.claude.argv();
    match resolve(&argv, "command") {
        Ok(full) => {
            let v = std::process::Command::new(&full[0])
                .args(&full[1..])
                .arg("--version")
                .stdin(std::process::Stdio::null())
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "version unknown".into());
            println!("claude:      {} ({v})", show_cmd(&full));
        }
        Err(_) => println!("claude:      NOT FOUND: {} (set [claude] command, or pass --claude)", show_cmd(&argv)),
    }
    let pull = cfg.claude.pull_argv();
    if pull != argv {
        match resolve(&pull, "pull_command") {
            Ok(full) => println!("pull:        {}", show_cmd(&full)),
            Err(_) => println!("pull:        NOT FOUND: {}", show_cmd(&pull)),
        }
    }
    match git::available() {
        Some(v) => {
            let repo = git::repo_root(&dir).map(|r| r.display().to_string()).unwrap_or_else(|| "not a git repo".into());
            println!("git:         {v} · {repo}");
        }
        None => println!("git:         not found (worktree threads disabled)"),
    }
    println!("config:      {}", config::config_path().map(|p| p.display().to_string()).unwrap_or_default());
    println!("folder:      {}", dir.display());
    let st = match state::load(&p.state_file) {
        Loaded::Missing => "none yet".to_string(),
        Loaded::Ok(s) => format!(
            "{} generation(s), {} thread(s) in the active one",
            s.generations.len(),
            s.threads().len()
        ),
        Loaded::Untouchable(w) => format!("UNUSABLE: {w}"),
    };
    println!("state file:  {} ({st})", p.state_file.display());
    println!(
        "hook relay:  {} ({})",
        p.socket.display(),
        if hooks::probe(&p.socket) { "cthread is running here" } else { "not running" }
    );
    let exe = std::env::current_exe().map(|e| e.display().to_string()).unwrap_or_default();
    println!("hook cmd:    {} hook", hooks::sh_quote(&exe));
    println!("transcripts: {}", paths::transcript_dir(&dir).display());
    Ok(())
}

fn prune(older: &str) -> Result<()> {
    let d = state::parse_duration(older).with_context(|| format!("bad duration {older:?} (use e.g. 30d, 12h)"))?;
    let p = ProjectPaths::new(&project_dir()?);
    let _lock = state::acquire_lock(&p.lock_file).map_err(|_| anyhow::anyhow!("cthread is running in this folder; quit it first"))?;
    match state::load(&p.state_file) {
        Loaded::Missing => println!("no state for this folder"),
        Loaded::Untouchable(w) => bail!("{w}"),
        Loaded::Ok(mut s) => {
            let n = s.prune(chrono::Utc::now() - d);
            state::save(&p.state_file, &s)?;
            println!("removed {n} generation(s) older than {older}");
        }
    }
    Ok(())
}

struct TermGuard {
    mouse: bool,
}

impl TermGuard {
    fn enter(mouse: bool) -> Result<TermGuard> {
        terminal::enable_raw_mode()?;
        let mut out = stdout();
        execute!(out, EnterAlternateScreen, EnableBracketedPaste)?;
        if mouse {
            execute!(out, EnableMouseCapture)?;
        }
        Ok(TermGuard { mouse })
    }

    fn leave(&self) {
        let mut out = stdout();
        if self.mouse {
            let _ = execute!(out, DisableMouseCapture);
        }
        let _ = execute!(out, DisableBracketedPaste, LeaveAlternateScreen, crossterm::cursor::Show);
        let _ = terminal::disable_raw_mode();
    }
}

impl Drop for TermGuard {
    fn drop(&mut self) {
        self.leave();
    }
}

fn run(claude: Option<String>) -> Result<()> {
    let (mut cfg, warnings) = load_config(claude)?;
    // FR-2 preflight, before entering the TUI: both commands must resolve.
    let interactive = resolve(&cfg.claude.argv(), "command")?;
    let pull = resolve(&cfg.claude.pull_argv(), "pull_command")?;
    cfg.claude.command = Some(config::CommandLine::Argv(interactive));
    cfg.claude.pull_command = Some(config::CommandLine::Argv(pull));
    let (w, h) = terminal::size().context("not running in a terminal")?;
    if w < layout::MIN_W || h < layout::MIN_H {
        bail!("terminal is {w}x{h}; cthread needs at least {}x{}", layout::MIN_W, layout::MIN_H);
    }

    let dir = project_dir()?;
    let p = ProjectPaths::new(&dir);
    let _lock = state::acquire_lock(&p.lock_file)
        .map_err(|_| anyhow::anyhow!("already running in {}", dir.display()))?;
    let mut warn = warnings;
    let store = match state::load(&p.state_file) {
        Loaded::Missing => Store::new(p.state_file.clone(), State::new(&dir), false),
        Loaded::Ok(s) => Store::new(p.state_file.clone(), s, false),
        Loaded::Untouchable(w) => {
            warn.push(w);
            Store::new(p.state_file.clone(), State::new(&dir), true)
        }
    };

    let (tx, rx) = mpsc::channel::<AppEvent>();
    let tx = Arc::new(std::sync::Mutex::new(tx));
    let notify: cthread::app::Notify = {
        let tx = tx.clone();
        Arc::new(move |e| {
            let _ = tx.lock().unwrap().send(e);
        })
    };
    let exe = std::env::current_exe().context("cannot find own executable")?;
    let mouse = cfg.ui.mouse;
    let mut app = App::new(cfg, p, store, AppOptions { hook_exe: exe }, notify)?;

    let guard = TermGuard::enter(mouse)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;
    let size = terminal.size()?;
    if let Err(e) = app.start((size.width, size.height)) {
        drop(guard);
        return Err(e);
    }
    if let Some(w) = warn.first() {
        app.set_status(w.clone());
    }

    // Terminal input thread; paused while $EDITOR owns the terminal.
    let paused = Arc::new(AtomicBool::new(false));
    {
        let tx = tx.clone();
        let paused = paused.clone();
        std::thread::spawn(move || {
            loop {
                if paused.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(30));
                    continue;
                }
                match event::poll(Duration::from_millis(30)) {
                    Ok(true) => {
                        if let Ok(e) = event::read()
                            && tx.lock().unwrap().send(AppEvent::Term(e)).is_err() {
                                break;
                            }
                    }
                    Ok(false) => {}
                    Err(_) => break,
                }
            }
        });
    }

    let frame_time = Duration::from_millis(16);
    let mut last_draw = Instant::now() - frame_time;
    loop {
        let wait = if app.dirty { frame_time.saturating_sub(last_draw.elapsed()) } else { Duration::from_millis(100) };
        if let Ok(ev) = rx.recv_timeout(wait) {
            app.handle(ev);
            while let Ok(ev) = rx.try_recv() {
                app.handle(ev);
                if app.quit {
                    break;
                }
            }
        }
        let now = Instant::now();
        app.tick(now);
        if app.quit {
            break;
        }
        if app.bell {
            app.bell = false;
            let _ = stdout().write_all(b"\x07");
            let _ = stdout().flush();
        }
        if let Some(req) = app.editor_request.take() {
            paused.store(true, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(40));
            guard.leave();
            let r = run_editor(&req.text);
            let _ = TermGuard::enter(mouse).map(std::mem::forget);
            let _ = terminal.clear();
            paused.store(false, Ordering::Relaxed);
            app.editor_done(r);
        }
        if app.dirty && last_draw.elapsed() >= frame_time {
            app.dirty = false;
            terminal.draw(|f| ui::draw(f, &mut app))?;
            app.sync_sizes();
            last_draw = Instant::now();
        }
    }
    drop(terminal);
    drop(guard);
    eprint!("cthread: stopping sessions…\r");
    app.shutdown();
    eprint!("                            \r");
    Ok(())
}

fn run_editor(text: &str) -> Result<String, String> {
    let editor = std::env::var("VISUAL").or_else(|_| std::env::var("EDITOR")).unwrap_or_else(|_| "vi".into());
    let dir = std::env::temp_dir();
    let path = dir.join(format!("cthread-pull-{}.md", std::process::id()));
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    let status = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(&path)
        .status()
        .map_err(|e| e.to_string())?;
    let out = std::fs::read_to_string(&path).map_err(|e| e.to_string());
    let _ = std::fs::remove_file(&path);
    if !status.success() {
        return Err(format!("{editor} exited with {status}"));
    }
    out
}
