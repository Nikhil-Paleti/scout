//! The manifest names the segments that make up one consistent snapshot.
//! It is replaced atomically (write temp, fsync, rename), so readers never
//! need a lock.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const FORMAT: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SegmentInfo {
    /// File name of the segment inside the index directory.
    pub name: String,
    /// Number of files in the segment, tombstoned or not.
    pub files: u32,
    /// "base" for full builds and compactions, "delta" for incremental ones.
    pub kind: String,
    /// File name of the current tombstone bitmap, if any file is deleted.
    #[serde(default)]
    pub del: Option<String>,
    /// Number of tombstoned files.
    #[serde(default)]
    pub deleted: u32,
    /// Whether a `.sym` sidecar with this segment's symbols exists.
    #[serde(default)]
    pub syms: bool,
}

impl SegmentInfo {
    pub fn new(name: String, files: u32, kind: &str, syms: bool) -> SegmentInfo {
        SegmentInfo {
            name,
            files,
            kind: kind.to_string(),
            del: None,
            deleted: 0,
            syms,
        }
    }

    pub fn sym_name(&self) -> String {
        format!("{}.sym", self.name.trim_end_matches(".idx"))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub format: u32,
    /// Canonical repository root this index covers.
    pub root: String,
    /// Git commit HEAD pointed at when the index was last reconciled.
    #[serde(default)]
    pub commit: Option<String>,
    /// Per-machine exclusion globs the index was built with.
    #[serde(default)]
    pub excludes: Vec<String>,
    /// Bumped on every manifest swap.
    pub generation: u64,
    /// Next segment number to hand out.
    pub next_seg: u64,
    /// Oldest first; a path in a later segment supersedes earlier copies.
    pub segments: Vec<SegmentInfo>,
    /// Unix seconds of the last full build.
    #[serde(default)]
    pub built_at: u64,
    /// Unix seconds of the last change applied.
    #[serde(default)]
    pub updated_at: u64,
    /// Whether files are indexed with symbols (definitions and calls).
    #[serde(default)]
    pub symbols: bool,
}

impl Manifest {
    pub fn new(root: &Path, excludes: Vec<String>) -> Manifest {
        Manifest {
            format: FORMAT,
            root: root.to_string_lossy().into_owned(),
            commit: None,
            excludes,
            generation: 0,
            next_seg: 1,
            segments: Vec::new(),
            built_at: 0,
            updated_at: 0,
            symbols: false,
        }
    }

    pub fn path(dir: &Path) -> PathBuf {
        dir.join("manifest.json")
    }

    pub fn load(dir: &Path) -> Result<Manifest> {
        let p = Self::path(dir);
        let data = fs::read(&p).with_context(|| format!("reading {}", p.display()))?;
        let m: Manifest =
            serde_json::from_slice(&data).with_context(|| format!("parsing {}", p.display()))?;
        if m.format != FORMAT {
            anyhow::bail!(
                "index format {} is not supported; run `scout build`",
                m.format
            );
        }
        Ok(m)
    }

    pub fn exists(dir: &Path) -> bool {
        Self::path(dir).exists()
    }

    /// Atomically replaces the manifest.
    pub fn store(&self, dir: &Path) -> Result<()> {
        let tmp = dir.join(format!("manifest.json.tmp.{}", std::process::id()));
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&serde_json::to_vec_pretty(self)?)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, Self::path(dir))?;
        Ok(())
    }

    pub fn alloc_segment_name(&mut self) -> String {
        let n = self.next_seg;
        self.next_seg += 1;
        format!("seg-{:06}.idx", n)
    }

    pub fn total_files(&self) -> u64 {
        self.segments.iter().map(|s| s.files as u64).sum()
    }

    pub fn live_files(&self) -> u64 {
        self.segments
            .iter()
            .map(|s| (s.files - s.deleted) as u64)
            .sum()
    }
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Base directory for all indexes: `$SCOUT_CACHE_DIR`, else
/// `$XDG_CACHE_HOME/scout`, else `~/.cache/scout`.
pub fn cache_base() -> PathBuf {
    if let Some(d) = std::env::var_os("SCOUT_CACHE_DIR") {
        return PathBuf::from(d);
    }
    if let Some(d) = std::env::var_os("XDG_CACHE_HOME") {
        if !d.is_empty() {
            return PathBuf::from(d).join("scout");
        }
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join(".cache").join("scout")
}

/// Index directory for a canonical repo root: `<cache>/<xxh3 of root>`.
pub fn index_dir_for(root: &Path) -> PathBuf {
    let h = xxhash_rust::xxh3::xxh3_64(root.as_os_str().as_encoded_bytes());
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(32)
        .collect();
    cache_base().join(format!("{}-{:016x}", name, h))
}

/// Creates the index dir with mode 0700.
pub fn ensure_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// Removes files in `dir` that look like index files but are not referenced
/// by `m`. Open readers keep unlinked files alive until they close them.
pub fn gc(dir: &Path, m: &Manifest) {
    let mut keep: std::collections::HashSet<String> = std::collections::HashSet::new();
    for s in &m.segments {
        keep.insert(s.name.clone());
        if s.syms {
            keep.insert(s.sym_name());
        }
        if let Some(d) = &s.del {
            keep.insert(d.clone());
        }
    }
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with("seg-") && !keep.contains(&name) {
                let _ = fs::remove_file(e.path());
            }
        }
    }
}
