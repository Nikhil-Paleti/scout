//! The per-repo daemon: keeps the index fresh with delta segments.
//!
//! One daemon per index directory (guarded by `daemon.lock`), listening on
//! `daemon.sock` (mode 0600) in that directory. Line protocol:
//!
//! ```text
//! notify <path>        a file changed (absolute or root-relative)
//! reconcile [full]     re-check git's dirty set (or stat-walk everything)
//! rebuild              full rebuild
//! status               one JSON line back
//! ping                 "pong" back
//! shutdown
//! ```
//!
//! No network code: only a Unix socket, std threads and `git` child
//! processes.

use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::json;

use scout_index::index::{apply_changes, ApplyStats, Index};
use scout_index::lock;
use scout_index::manifest::{self, Manifest, SegmentInfo};
use scout_index::merge::{merge, merge_has_syms, MergeInput};
use scout_index::{build, filter, BuildOptions, Rules};

use crate::{priority, repo};

const DEBOUNCE: Duration = Duration::from_millis(200);
const MAX_DELAY: Duration = Duration::from_millis(1000);
const GIT_POLL: Duration = Duration::from_secs(2);
const MAX_DELTAS: usize = 8;

pub fn socket_path(dir: &Path) -> PathBuf {
    dir.join("daemon.sock")
}

enum Msg {
    Paths(Vec<PathBuf>),
    Reconcile { full: bool },
    Rebuild,
    Status(Sender<String>),
    Shutdown,
}

pub struct ServeOptions {
    pub idle: Duration,
    pub excludes: Option<Vec<String>>,
    pub threads: usize,
}

/// Sends protocol lines; returns the reply line if `want_reply`.
pub fn send(dir: &Path, lines: &[String], want_reply: bool) -> Result<Option<String>> {
    let mut s = UnixStream::connect(socket_path(dir)).context("daemon not running")?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    s.set_write_timeout(Some(Duration::from_secs(5)))?;
    for l in lines {
        s.write_all(l.as_bytes())?;
        s.write_all(b"\n")?;
    }
    if !want_reply {
        return Ok(None);
    }
    s.shutdown(std::net::Shutdown::Write)?;
    let mut reply = String::new();
    BufReader::new(s).read_line(&mut reply)?;
    Ok(Some(reply.trim_end().to_string()))
}

pub fn is_running(dir: &Path) -> bool {
    matches!(send(dir, &["ping".to_string()], true), Ok(Some(r)) if r == "pong")
}

/// Starts `scout serve --foreground` detached from the caller's session.
pub fn spawn_detached(
    root: &Path,
    dir: &Path,
    excludes: &Option<Vec<String>>,
    idle: Duration,
) -> Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    manifest::ensure_dir(dir)?;
    if is_running(dir) {
        return Ok(());
    }
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("daemon.log"))?;
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.arg("serve").arg("--foreground").arg("--root").arg(root);
    cmd.arg("--idle-secs").arg(idle.as_secs().to_string());
    if let Some(ex) = excludes {
        if ex.is_empty() {
            cmd.arg("--no-excludes");
        }
        for e in ex {
            cmd.arg("--exclude").arg(e);
        }
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log));
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().context("starting daemon")?;
    Ok(())
}

fn log(msg: impl AsRef<str>) {
    let t = manifest::now_secs();
    eprintln!("[{}] {}", t, msg.as_ref());
}

fn handle_conn(stream: UnixStream, tx: Sender<Msg>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let line = line.trim_end();
        let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
        match cmd {
            "notify" if !arg.is_empty() => {
                let _ = tx.send(Msg::Paths(vec![PathBuf::from(arg)]));
            }
            "reconcile" => {
                let _ = tx.send(Msg::Reconcile {
                    full: arg == "full",
                });
            }
            "rebuild" => {
                let _ = tx.send(Msg::Rebuild);
            }
            "ping" => {
                let _ = writer.write_all(b"pong\n");
            }
            "status" => {
                let (rtx, rrx) = mpsc::channel();
                if tx.send(Msg::Status(rtx)).is_ok() {
                    if let Ok(s) = rrx.recv_timeout(Duration::from_secs(30)) {
                        let _ = writer.write_all(s.as_bytes());
                        let _ = writer.write_all(b"\n");
                    }
                }
            }
            "shutdown" => {
                let _ = tx.send(Msg::Shutdown);
            }
            _ => {
                let _ = writer.write_all(b"error unknown command\n");
            }
        }
    }
}

struct Worker {
    root: PathBuf,
    dir: PathBuf,
    rules: Rules,
    threads: usize,
    pending: BTreeSet<PathBuf>,
    first_pending: Option<Instant>,
    last_msg: Instant,
    last_activity: Instant,
    started: Instant,
    last_apply: Option<(u64, ApplyStats, Duration)>,
    applied_total: u64,
    git_stamp: Option<(i64, u64, i64, u64)>,
    last_git_check: Instant,
    /// HEAD as seen just before the last reconcile listed its paths; it is
    /// recorded in the manifest only once those paths are applied, so the
    /// stored commit never claims changes that weren't indexed.
    pending_commit: Option<String>,
    /// Background symbol pass, when one is running.
    sym_job: Option<std::thread::JoinHandle<()>>,
    sym_cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    last_sym_check: Instant,
}

impl Worker {
    /// Starts the symbol backfill in its own thread if the index needs it.
    fn maybe_symbolize(&mut self) {
        if let Some(j) = &self.sym_job {
            if !j.is_finished() {
                return;
            }
            self.sym_job = None;
        }
        if self.last_sym_check.elapsed() < Duration::from_secs(2) {
            return;
        }
        self.last_sym_check = Instant::now();
        if !scout_index::symbolize::needed(&self.dir) {
            return;
        }
        let (root, dir, threads, cancel) = (
            self.root.clone(),
            self.dir.clone(),
            self.threads,
            self.sym_cancel.clone(),
        );
        self.sym_job = Some(std::thread::spawn(
            move || match scout_index::symbolize::backfill(&root, &dir, threads, false, &cancel) {
                Ok(Some(s)) if s.segments > 0 => log(format!(
                    "symbols: {} from {} files in {:.1?} ({:.1} MB)",
                    s.symbols,
                    s.files,
                    s.time,
                    s.bytes as f64 / 1e6
                )),
                Ok(_) => {}
                Err(e) => log(format!("symbol indexing failed: {:#}", e)),
            },
        ));
    }

    fn symbolizing(&self) -> bool {
        self.sym_job
            .as_ref()
            .map(|j| !j.is_finished())
            .unwrap_or(false)
    }

    fn rel(&self, p: &Path) -> Option<PathBuf> {
        if p.is_absolute() {
            p.strip_prefix(&self.root).ok().map(|r| r.to_path_buf())
        } else {
            Some(p.to_path_buf())
        }
    }

    fn enqueue(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        let mut any = false;
        for p in paths {
            if let Some(r) = self.rel(&p) {
                if !r.as_os_str().is_empty() {
                    self.pending.insert(r);
                    any = true;
                }
            }
        }
        if any {
            self.first_pending.get_or_insert_with(Instant::now);
            self.last_msg = Instant::now();
        }
    }

    fn excludes(&self) -> Vec<String> {
        self.rules.excludes().to_vec()
    }

    fn rebuild(&mut self) -> Result<()> {
        let t = Instant::now();
        let opts = BuildOptions {
            threads: self.threads,
            excludes: self.excludes(),
            commit: repo::head(&self.root),
            ..Default::default()
        };
        let st = build::build_full(&self.root, &self.dir, &opts)?;
        self.pending.clear();
        self.first_pending = None;
        self.git_stamp = repo::git_stamp(&self.root);
        log(format!(
            "rebuilt: {} files in {:.1?}",
            st.files_indexed,
            t.elapsed()
        ));
        Ok(())
    }

    /// Paths git thinks differ from the indexed commit, plus its dirty set.
    fn git_reconcile(&mut self) -> Result<()> {
        let head_before = repo::head(&self.root);
        let m = Manifest::load(&self.dir)?;
        let mut paths = Vec::new();
        match m
            .commit
            .as_deref()
            .and_then(|c| repo::changed_since(&self.root, c))
        {
            Some(p) => paths.extend(p),
            None if repo::head(&self.root).is_some() && m.commit != repo::head(&self.root) => {
                // The indexed commit is unknown to git (gc'd, shallow): stat walk.
                return self.full_reconcile();
            }
            None => {}
        }
        match repo::status_paths(&self.root) {
            Some(p) => paths.extend(p),
            // git was busy (e.g. mid-commit holding index.lock): retry on the
            // next poll rather than miss the change.
            None if self.root.join(".git").exists() => bail!("git status failed"),
            None => {}
        }
        self.enqueue(paths);
        self.pending_commit = head_before;
        // Record the new HEAD even if no files changed.
        self.first_pending.get_or_insert_with(Instant::now);
        Ok(())
    }

    /// Compares a fresh walk against the index by size and mtime.
    fn full_reconcile(&mut self) -> Result<()> {
        let head_before = repo::head(&self.root);
        self.rules.reload();
        let index = Index::open(&self.dir)?;
        let skipped = std::sync::atomic::AtomicU64::new(0);
        let on_disk = build::walk(
            &self.rules,
            self.threads,
            filter::DEFAULT_MAX_FILE_SIZE,
            &skipped,
        );
        let mut changed: Vec<PathBuf> = Vec::new();
        let mut seen: BTreeSet<Vec<u8>> = BTreeSet::new();
        for rel in &on_disk {
            let key = rel.as_os_str().as_encoded_bytes().to_vec();
            match index.lookup(&key) {
                Some((si, id)) => {
                    let m = index.segments[si].seg.meta(id);
                    let md = fs::symlink_metadata(self.root.join(rel));
                    let same = md
                        .map(|md| {
                            md.len() == m.size && scout_index::segment::mtime_ns(&md) == m.mtime_ns
                        })
                        .unwrap_or(false);
                    if !same {
                        changed.push(rel.clone());
                    }
                }
                None => changed.push(rel.clone()),
            }
            seen.insert(key);
        }
        for f in index.live_files() {
            if !seen.contains(f.path) {
                changed.push(PathBuf::from(String::from_utf8_lossy(f.path).into_owned()));
            }
        }
        log(format!(
            "full reconcile: {} paths to re-check",
            changed.len()
        ));
        self.enqueue(changed);
        self.pending_commit = head_before;
        self.first_pending.get_or_insert_with(Instant::now);
        Ok(())
    }

    fn apply(&mut self) -> Result<()> {
        let paths: Vec<PathBuf> = std::mem::take(&mut self.pending).into_iter().collect();
        self.first_pending = None;
        let ignore_changed = paths.iter().any(|p| Rules::is_ignore_file(p));
        let t = Instant::now();
        let commit = self.pending_commit.take();
        let stats = {
            let _g = lock::lock(&self.dir.join(lock::WRITE_LOCK))?;
            apply_changes(
                &self.dir,
                &self.rules,
                &paths,
                filter::DEFAULT_MAX_FILE_SIZE,
                commit,
            )?
        };
        if stats.indexed + stats.removed > 0 {
            log(format!(
                "applied {} paths: {} indexed, {} removed, {} unchanged in {:.1?}",
                stats.examined,
                stats.indexed,
                stats.removed,
                stats.unchanged,
                t.elapsed()
            ));
        }
        self.applied_total += (stats.indexed + stats.removed) as u64;
        self.last_apply = Some((manifest::now_secs(), stats, t.elapsed()));
        self.last_activity = Instant::now();
        self.maybe_compact()?;
        if ignore_changed {
            log("an ignore file changed; reconciling the whole tree");
            self.full_reconcile()?;
        }
        Ok(())
    }

    fn maybe_compact(&mut self) -> Result<()> {
        let m = Manifest::load(&self.dir)?;
        let base_files: u64 = m
            .segments
            .iter()
            .filter(|s| s.kind == "base")
            .map(|s| s.files as u64)
            .sum();
        let deltas: Vec<usize> = (0..m.segments.len())
            .filter(|&i| m.segments[i].kind == "delta")
            .collect();
        let delta_files: u64 = deltas.iter().map(|&i| m.segments[i].files as u64).sum();
        let first_delta = match deltas.first() {
            Some(&i) => i,
            None => return Ok(()),
        };
        if delta_files * 20 > base_files.max(1) && delta_files > 64 {
            self.compact(0, "base")
        } else if deltas.len() > MAX_DELTAS {
            self.compact(first_delta, "delta")
        } else {
            Ok(())
        }
    }

    /// Merges segments `from..` into one.
    fn compact(&mut self, from: usize, kind: &str) -> Result<()> {
        let t = Instant::now();
        let _g = lock::lock(&self.dir.join(lock::WRITE_LOCK))?;
        let index = Index::open(&self.dir)?;
        let mut m = index.manifest.clone();
        let name = m.alloc_segment_name();
        let inputs: Vec<MergeInput> = index.segments[from..]
            .iter()
            .map(|s| MergeInput {
                seg: &s.seg,
                del: if s.del.is_empty() { None } else { Some(&s.del) },
                syms: s.syms.as_ref(),
            })
            .collect();
        let syms = merge_has_syms(&inputs);
        let files = merge(&inputs, &self.dir.join(&name))?;
        drop(inputs);
        m.segments.truncate(from);
        m.segments.push(SegmentInfo::new(name, files, kind, syms));
        m.generation += 1;
        m.updated_at = manifest::now_secs();
        m.store(&self.dir)?;
        drop(index);
        manifest::gc(&self.dir, &m);
        log(format!(
            "compacted into one {} segment ({} files) in {:.1?}",
            kind,
            files,
            t.elapsed()
        ));
        Ok(())
    }

    fn status(&self) -> String {
        let m = Manifest::load(&self.dir).ok();
        json!({
            "pid": std::process::id(),
            "root": self.root,
            "uptime_secs": self.started.elapsed().as_secs(),
            "pending": self.pending.len(),
            "symbolizing": self.symbolizing(),
            "applied_total": self.applied_total,
            "generation": m.as_ref().map(|m| m.generation),
            "last_apply": self.last_apply.as_ref().map(|(at, s, d)| json!({
                "at": at, "examined": s.examined, "indexed": s.indexed,
                "removed": s.removed, "ms": d.as_millis() as u64,
            })),
        })
        .to_string()
    }
}

pub fn serve(root: &Path, dir: &Path, opts: ServeOptions) -> Result<()> {
    manifest::ensure_dir(dir)?;
    let Some(_daemon_lock) = lock::try_lock(&dir.join(lock::DAEMON_LOCK))? else {
        log("another daemon already serves this index");
        return Ok(());
    };
    priority::background();
    fs::write(dir.join("daemon.pid"), std::process::id().to_string())?;
    let sock = socket_path(dir);
    if sock.as_os_str().len() > 100 {
        bail!(
            "socket path too long: {} (set SCOUT_CACHE_DIR to a shorter path)",
            sock.display()
        );
    }
    let _ = fs::remove_file(&sock);
    let listener =
        UnixListener::bind(&sock).with_context(|| format!("binding {}", sock.display()))?;
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&sock, fs::Permissions::from_mode(0o600))?;
    }
    log(format!(
        "serving {} (pid {})",
        root.display(),
        std::process::id()
    ));

    let (tx, rx) = mpsc::channel::<Msg>();
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for s in listener.incoming().flatten() {
                let tx = tx.clone();
                std::thread::spawn(move || handle_conn(s, tx));
            }
        });
    }
    drop(tx);

    let existing = Manifest::load(dir).ok();
    let excludes = opts
        .excludes
        .clone()
        .or_else(|| existing.as_ref().map(|m| m.excludes.clone()))
        .unwrap_or_default();
    let now = Instant::now();
    let mut w = Worker {
        root: root.to_path_buf(),
        dir: dir.to_path_buf(),
        rules: Rules::new(root, &excludes)?,
        threads: opts.threads,
        pending: BTreeSet::new(),
        first_pending: None,
        last_msg: now,
        last_activity: now,
        started: now,
        last_apply: None,
        applied_total: 0,
        git_stamp: repo::git_stamp(root),
        last_git_check: now,
        pending_commit: None,
        sym_job: None,
        sym_cancel: Default::default(),
        last_sym_check: now - Duration::from_secs(60),
    };
    match existing {
        Some(m) if m.excludes == excludes => {
            // Startup reconcile: whatever changed while nobody was watching.
            if let Err(e) = w.git_reconcile() {
                log(format!("startup reconcile failed: {:#}", e));
            }
            if repo::head(root).is_none() {
                w.full_reconcile()?;
            }
        }
        _ => w.rebuild()?,
    }

    loop {
        let timeout = if w.pending.is_empty() {
            Duration::from_millis(500)
        } else {
            Duration::from_millis(50)
        };
        match rx.recv_timeout(timeout) {
            Ok(Msg::Paths(p)) => {
                w.last_activity = Instant::now();
                w.enqueue(p);
            }
            Ok(Msg::Reconcile { full }) => {
                w.last_activity = Instant::now();
                let r = if full {
                    w.full_reconcile()
                } else {
                    w.git_reconcile()
                };
                if let Err(e) = r {
                    log(format!("reconcile failed: {:#}", e));
                }
            }
            Ok(Msg::Rebuild) => {
                w.last_activity = Instant::now();
                if let Err(e) = w.rebuild() {
                    log(format!("rebuild failed: {:#}", e));
                }
            }
            Ok(Msg::Status(reply)) => {
                let _ = reply.send(w.status());
            }
            Ok(Msg::Shutdown) => break,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if let Some(first) = w.first_pending {
            if w.last_msg.elapsed() >= DEBOUNCE || first.elapsed() >= MAX_DELAY {
                if let Err(e) = w.apply() {
                    log(format!("update failed: {:#}", e));
                }
            }
        }
        if w.last_git_check.elapsed() >= GIT_POLL {
            w.last_git_check = Instant::now();
            let stamp = repo::git_stamp(root);
            let busy = root.join(".git/index.lock").exists();
            if stamp != w.git_stamp && !busy {
                w.last_activity = Instant::now();
                // Only record the stamp once reconcile succeeded, so a change
                // made while git was busy is picked up on the next poll.
                match w.git_reconcile() {
                    Ok(()) => w.git_stamp = stamp,
                    Err(e) => log(format!("git reconcile failed, will retry: {:#}", e)),
                }
            }
        }
        w.maybe_symbolize();
        if w.pending.is_empty() && !w.symbolizing() && w.last_activity.elapsed() >= opts.idle {
            log("idle, exiting");
            break;
        }
    }
    w.sym_cancel
        .store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some(j) = w.sym_job.take() {
        let _ = j.join();
    }
    let _ = fs::remove_file(&sock);
    let _ = fs::remove_file(dir.join("daemon.pid"));
    Ok(())
}
