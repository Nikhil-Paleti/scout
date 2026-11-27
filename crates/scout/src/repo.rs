//! Repository root discovery and the few git commands the daemon runs.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

/// The repo root: `--root`, else the nearest ancestor of the current
/// directory holding `.git`, else the current directory. Canonicalized.
pub fn find_root(explicit: Option<&str>) -> Result<PathBuf> {
    if let Some(r) = explicit {
        return std::fs::canonicalize(r).with_context(|| format!("root {}", r));
    }
    let cwd = std::env::current_dir()?.canonicalize()?;
    let mut d: Option<&Path> = Some(&cwd);
    while let Some(dir) = d {
        if dir.join(".git").exists() {
            return Ok(dir.to_path_buf());
        }
        d = dir.parent();
    }
    Ok(cwd)
}

fn git(root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    if !root.join(".git").exists() {
        return None;
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if out.status.success() {
        Some(out.stdout)
    } else {
        None
    }
}

pub fn head(root: &Path) -> Option<String> {
    let out = git(root, &["rev-parse", "--verify", "-q", "HEAD"])?;
    let s = String::from_utf8_lossy(&out).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn split_z(out: &[u8]) -> Vec<PathBuf> {
    out.split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| PathBuf::from(String::from_utf8_lossy(s).into_owned()))
        .collect()
}

/// Files that differ between `commit` and the working tree. None if git
/// can't answer (no repo, commit gone after a shallow fetch or gc).
pub fn changed_since(root: &Path, commit: &str) -> Option<Vec<PathBuf>> {
    let out = git(
        root,
        &["diff", "--name-only", "--no-renames", "-z", commit, "--"],
    )?;
    Some(split_z(&out))
}

/// Modified, staged, deleted and untracked (not ignored) files.
pub fn status_paths(root: &Path) -> Option<Vec<PathBuf>> {
    let out = git(
        root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--no-renames",
        ],
    )?;
    let mut paths = Vec::new();
    for rec in out.split(|&b| b == 0) {
        if rec.len() > 3 {
            paths.push(PathBuf::from(
                String::from_utf8_lossy(&rec[3..]).into_owned(),
            ));
        }
    }
    Some(paths)
}

/// (mtime, len) of `.git/HEAD` and `.git/index`, to notice git activity.
pub fn git_stamp(root: &Path) -> Option<(i64, u64, i64, u64)> {
    let g = root.join(".git");
    if !g.is_dir() {
        return None;
    }
    let st = |p: PathBuf| {
        std::fs::metadata(p)
            .map(|m| (scout_index::segment::mtime_ns(&m), m.len()))
            .unwrap_or((0, 0))
    };
    let (a, b) = st(g.join("HEAD"));
    let (c, d) = st(g.join("index"));
    Some((a, b, c, d))
}
