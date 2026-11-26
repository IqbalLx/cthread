//! `~/.config/cthread/config.toml`. Every key has a default; invalid values
//! fall back to defaults with a warning.

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    pub keys: Keys,
    pub ui: Ui,
    pub threads: Threads,
    pub worktree: Worktree,
    pub claude: Claude,
    pub pull: Pull,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Keys {
    /// One key or a list of keys; any of them starts a command. The first is
    /// the one shown in hints.
    #[serde(deserialize_with = "one_or_many")]
    pub prefix: Vec<String>,
    /// Command key per action, e.g. `new_thread = "t"` or `pull = ["p", "C-p"]`.
    #[serde(deserialize_with = "map_one_or_many")]
    pub commands: std::collections::BTreeMap<String, Vec<String>>,
    /// Built from `commands` during validation.
    #[serde(skip)]
    pub keymap: crate::keymap::Keymap,
}

fn map_one_or_many<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<std::collections::BTreeMap<String, Vec<String>>, D::Error> {
    #[derive(Deserialize)]
    struct W(#[serde(deserialize_with = "one_or_many")] Vec<String>);
    let m: std::collections::BTreeMap<String, W> = Deserialize::deserialize(d)?;
    Ok(m.into_iter().map(|(k, W(v))| (k, v)).collect())
}

fn one_or_many<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(d)? {
        OneOrMany::One(s) => vec![s],
        OneOrMany::Many(v) => v,
    })
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Ui {
    pub sidebar_width: u16,
    pub narrow_breakpoint: u16,
    pub mouse: bool,
    pub bell_on_needs_you: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Threads {
    pub discard_after_minutes: u64,
    pub max_live: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Worktree {
    /// Relative to the repo root (or absolute).
    pub root: String,
    pub branch_prefix: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct Claude {
    /// The command that starts Claude Code: a program plus optional leading
    /// arguments, as a shell-style string (`"ccr code"`) or an argv array
    /// (`["ccr", "code"]`). cthread appends its own flags after it.
    pub command: Option<CommandLine>,
    /// Command for pull summaries (`-p` runs). Defaults to `command`.
    pub pull_command: Option<CommandLine>,
    /// Older name for `command` (a single executable). Used when `command`
    /// is not set.
    pub path: Option<String>,
    /// Appended to every interactive launch, e.g. `["--model", "opus"]`.
    pub extra_args: Vec<String>,
}

/// A command line from the config or the `--claude` flag.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum CommandLine {
    /// Split like a shell would (quotes and backslashes), no expansion.
    Line(String),
    Argv(Vec<String>),
}

impl CommandLine {
    pub fn argv(&self) -> Result<Vec<String>, String> {
        let v = match self {
            CommandLine::Line(s) => shell_words::split(s).map_err(|e| format!("cannot parse {s:?}: {e}"))?,
            CommandLine::Argv(v) => v.clone(),
        };
        match v.first() {
            Some(p) if !p.trim().is_empty() => Ok(v),
            _ => Err("command is empty".into()),
        }
    }
}

impl Claude {
    /// argv that starts an interactive session (before cthread's own flags).
    pub fn argv(&self) -> Vec<String> {
        if let Some(c) = &self.command
            && let Ok(v) = c.argv() {
                return v;
            }
        vec![self.path.clone().filter(|p| !p.trim().is_empty()).unwrap_or_else(|| "claude".into())]
    }

    /// argv for pull summaries (before `-p --resume …`).
    pub fn pull_argv(&self) -> Vec<String> {
        self.pull_command.as_ref().and_then(|c| c.argv().ok()).unwrap_or_else(|| self.argv())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Pull {
    pub timeout_seconds: u64,
    pub prompt: String,
}

impl Default for Keys {
    fn default() -> Self {
        Self {
            prefix: vec!["C-q".into(), "C-]".into()],
            commands: Default::default(),
            keymap: Default::default(),
        }
    }
}
impl Default for Ui {
    fn default() -> Self {
        Self { sidebar_width: 22, narrow_breakpoint: 120, mouse: true, bell_on_needs_you: false }
    }
}
impl Default for Threads {
    fn default() -> Self {
        Self { discard_after_minutes: 10, max_live: 5 }
    }
}
impl Default for Worktree {
    fn default() -> Self {
        Self { root: "..".into(), branch_prefix: "cthread/".into() }
    }
}
impl Default for Pull {
    fn default() -> Self {
        Self {
            timeout_seconds: 120,
            prompt: "Summarize the conclusions, decisions, changed files and open questions of this conversation in under 200 words, as bullets.".into(),
        }
    }
}

pub fn config_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("CTHREAD_CONFIG") {
        return Some(PathBuf::from(p));
    }
    // The PRD names ~/.config on every platform (not ~/Library on macOS).
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME")
        && !x.is_empty() {
            return Some(PathBuf::from(x).join("cthread/config.toml"));
        }
    directories::BaseDirs::new().map(|b| b.home_dir().join(".config/cthread/config.toml"))
}

impl Config {
    /// Load config; returns the config and any warnings to show the user.
    pub fn load() -> (Config, Vec<String>) {
        match config_path() {
            Some(p) if p.exists() => match std::fs::read_to_string(&p) {
                Ok(s) => Self::parse(&s),
                Err(e) => (Config::default(), vec![format!("config: cannot read {}: {e}", p.display())]),
            },
            _ => (Config::default(), vec![]),
        }
    }

    pub fn parse(s: &str) -> (Config, Vec<String>) {
        let mut warnings = vec![];
        let mut cfg = match toml::from_str::<Config>(s) {
            Ok(c) => c,
            Err(e) => {
                warnings.push(format!("config: invalid file, using defaults ({})", e.message()));
                Config::default()
            }
        };
        cfg.validate(&mut warnings);
        (cfg, warnings)
    }

    fn validate(&mut self, w: &mut Vec<String>) {
        let d = Config::default();
        let mut keys = Vec::new();
        for k in std::mem::take(&mut self.keys.prefix) {
            if crate::keys::Prefix::parse(&k).is_some() {
                keys.push(k);
            } else {
                w.push(format!("config: keys.prefix {k:?} is not a key like \"C-q\" or \"C-]\"; ignoring it"));
            }
        }
        self.keys.prefix = if keys.is_empty() { d.keys.prefix } else { keys };
        self.keys.keymap = crate::keymap::Keymap::build(&self.keys.commands, w);
        if !(8..=60).contains(&self.ui.sidebar_width) {
            w.push("config: ui.sidebar_width must be 8..=60; using 22".into());
            self.ui.sidebar_width = d.ui.sidebar_width;
        }
        if self.threads.max_live == 0 {
            w.push("config: threads.max_live must be at least 1; using 5".into());
            self.threads.max_live = d.threads.max_live;
        }
        if self.pull.timeout_seconds == 0 {
            w.push("config: pull.timeout_seconds must be positive; using 120".into());
            self.pull.timeout_seconds = d.pull.timeout_seconds;
        }
        if self.pull.prompt.trim().is_empty() {
            self.pull.prompt = d.pull.prompt;
        }
        for (name, cmd) in [("command", &mut self.claude.command), ("pull_command", &mut self.claude.pull_command)] {
            if let Some(Err(e)) = cmd.as_ref().map(CommandLine::argv) {
                w.push(format!("config: claude.{name}: {e}; using the default"));
                *cmd = None;
            }
        }
        if self.worktree.branch_prefix.contains(char::is_whitespace) {
            w.push("config: worktree.branch_prefix may not contain spaces; using cthread/".into());
            self.worktree.branch_prefix = d.worktree.branch_prefix;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_default() {
        let (c, w) = Config::parse("");
        assert!(w.is_empty());
        assert_eq!(c.ui.sidebar_width, 22);
        assert_eq!(c.keys.prefix, vec!["C-q", "C-]"]);
        assert_eq!(c.threads.max_live, 5);
    }

    #[test]
    fn prefix_one_or_many() {
        let (c, w) = Config::parse("[keys]\nprefix = \"C-a\"\n");
        assert!(w.is_empty());
        assert_eq!(c.keys.prefix, vec!["C-a"]);
        let (c, w) = Config::parse("[keys]\nprefix = [\"C-Space\", \"bogus\", \"C-]\"]\n");
        assert_eq!(c.keys.prefix, vec!["C-Space", "C-]"]);
        assert_eq!(w.len(), 1);
        let (c, w) = Config::parse("[keys]\nprefix = []\n");
        assert!(w.is_empty());
        assert_eq!(c.keys.prefix, vec!["C-q", "C-]"]);
    }

    #[test]
    fn command_keys() {
        use crate::keymap::Action;
        let (c, w) = Config::parse(
            "[keys]\nprefix = \"C-a\"\n[keys.commands]\nnew_thread = \"t\"\npull = [\"P\", \"C-p\"]\nnope = \"z\"\n",
        );
        assert_eq!(w.len(), 1, "{w:?}");
        assert_eq!(c.keys.keymap.label(Action::NewThread), "t");
        assert_eq!(c.keys.keymap.label(Action::Pull), "P");
        assert_eq!(c.keys.keymap.label(Action::Quit), "q");
    }

    #[test]
    fn partial_and_invalid() {
        let (c, w) = Config::parse("[ui]\nsidebar_width = 2\nmouse = false\n[threads]\nmax_live = 0\n");
        assert_eq!(c.ui.sidebar_width, 22);
        assert!(!c.ui.mouse);
        assert_eq!(c.threads.max_live, 5);
        assert_eq!(w.len(), 2);
    }

    #[test]
    fn claude_command() {
        let (c, w) = Config::parse("");
        assert!(w.is_empty());
        assert_eq!(c.claude.argv(), vec!["claude"]);
        assert_eq!(c.claude.pull_argv(), vec!["claude"]);

        // string form: shell-style splitting with quotes
        let (c, _) = Config::parse("[claude]\ncommand = \"ccr code --profile 'my work'\"\n");
        assert_eq!(c.claude.argv(), vec!["ccr", "code", "--profile", "my work"]);
        assert_eq!(c.claude.pull_argv(), c.claude.argv());

        // array form, plus a separate pull command
        let (c, _) = Config::parse("[claude]\ncommand = [\"/opt/my claude/bin\", \"--x\"]\npull_command = \"claude\"\n");
        assert_eq!(c.claude.argv(), vec!["/opt/my claude/bin", "--x"]);
        assert_eq!(c.claude.pull_argv(), vec!["claude"]);

        // legacy `path`
        let (c, _) = Config::parse("[claude]\npath = \"/usr/local/bin/claude\"\n");
        assert_eq!(c.claude.argv(), vec!["/usr/local/bin/claude"]);

        // invalid → warning + default
        let (c, w) = Config::parse("[claude]\ncommand = \"ccr 'unclosed\"\npull_command = []\n");
        assert_eq!(w.len(), 2, "{w:?}");
        assert_eq!(c.claude.argv(), vec!["claude"]);
        assert_eq!(c.claude.pull_argv(), vec!["claude"]);
    }

    #[test]
    fn broken_toml() {
        let (c, w) = Config::parse("[ui\nfoo");
        assert_eq!(c.ui.sidebar_width, 22);
        assert_eq!(w.len(), 1);
    }
}
