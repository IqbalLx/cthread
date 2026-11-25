//! Git CLI helpers for worktree threads (FR-50..FR-53).

use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()?;
    if !out.status.success() {
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

pub fn available() -> Option<String> {
    let out = Command::new("git").arg("--version").stdin(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Top-level folder of the repository containing `dir`, if any.
pub fn repo_root(dir: &Path) -> Option<PathBuf> {
    git(dir, &["rev-parse", "--show-toplevel"]).ok().map(PathBuf::from)
}

/// Whether the repository has at least one commit (a worktree needs HEAD).
pub fn has_head(dir: &Path) -> bool {
    git(dir, &["rev-parse", "--verify", "-q", "HEAD"]).is_ok()
}

/// Uncommitted changes (including untracked files)?
pub fn is_dirty(dir: &Path) -> Result<bool> {
    Ok(!git(dir, &["status", "--porcelain"])?.is_empty())
}

fn branch_exists(dir: &Path, branch: &str) -> bool {
    git(dir, &["show-ref", "--verify", "-q", &format!("refs/heads/{branch}")]).is_ok()
}

/// Pick a free worktree path and branch for `slug`:
/// `<root>/<repo>-<slug>` and `<prefix><slug>`, adding `-2`, `-3`… on clashes.
pub fn plan_worktree(repo: &Path, root_cfg: &str, prefix: &str, slug: &str) -> (PathBuf, String) {
    let root = {
        let r = Path::new(root_cfg);
        if r.is_absolute() { r.to_path_buf() } else { repo.join(r) }
    };
    let repo_name = repo.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "repo".into());
    for n in 1.. {
        let s = if n == 1 { slug.to_string() } else { format!("{slug}-{n}") };
        let path = root.join(format!("{repo_name}-{s}"));
        let branch = format!("{prefix}{s}");
        if !path.exists() && !branch_exists(repo, &branch) {
            return (normalize(&path), branch);
        }
    }
    unreachable!()
}

/// Lexically normalize `a/b/../c` without touching the filesystem.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

/// `git worktree add -b <branch> <path> HEAD`.
pub fn add_worktree(repo: &Path, path: &Path, branch: &str) -> Result<()> {
    git(repo, &["worktree", "add", "-b", branch, &path.to_string_lossy(), "HEAD"])?;
    Ok(())
}

/// Remove a worktree and its branch. `force` removes it even when dirty.
pub fn remove_worktree(repo: &Path, path: &Path, branch: &str, force: bool) -> Result<()> {
    if path.exists() {
        let p = path.to_string_lossy();
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(&p);
        git(repo, &args)?;
    } else {
        let _ = git(repo, &["worktree", "prune"]);
    }
    if branch_exists(repo, branch) {
        git(repo, &["branch", "-D", branch])?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn init_repo(dir: &Path) {
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "t@example.com"],
        vec!["config", "user.name", "t"],
        vec!["commit", "-q", "--allow-empty", "-m", "init"],
    ] {
        git(dir, &args).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_lifecycle() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("shop");
        std::fs::create_dir(&repo).unwrap();
        assert!(repo_root(&repo).is_none() || repo_root(&repo).unwrap() != repo);
        init_repo(&repo);
        let repo = crate::paths::canonical(&repo);
        assert_eq!(repo_root(&repo).unwrap(), repo);
        assert!(has_head(&repo));
        assert!(!is_dirty(&repo).unwrap());

        let (path, branch) = plan_worktree(&repo, "..", "cthread/", "db-schema");
        assert_eq!(path, repo.parent().unwrap().join("shop-db-schema"));
        assert_eq!(branch, "cthread/db-schema");
        add_worktree(&repo, &path, &branch).unwrap();
        assert!(path.join(".git").exists());

        // clash → -2
        let (p2, b2) = plan_worktree(&repo, "..", "cthread/", "db-schema");
        assert!(p2.ends_with("shop-db-schema-2"));
        assert_eq!(b2, "cthread/db-schema-2");

        // edits in the worktree don't touch main's folder
        std::fs::write(path.join("x.txt"), "hi").unwrap();
        assert!(is_dirty(&path).unwrap());
        assert!(!is_dirty(&repo).unwrap());

        assert!(remove_worktree(&repo, &path, &branch, false).is_err());
        remove_worktree(&repo, &path, &branch, true).unwrap();
        assert!(!path.exists());
        assert!(!branch_exists(&repo, &branch));
    }
}
