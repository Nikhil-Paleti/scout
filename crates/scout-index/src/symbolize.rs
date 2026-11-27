//! Adds symbol sidecars to segments that lack them.
//!
//! Tree-sitter parsing costs roughly ten times as much as trigram
//! extraction, so a full build indexes text first and this pass follows,
//! with text search already usable. It runs in `scout build` or in the
//! daemon, whichever gets `symbols.lock` first.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rayon::prelude::*;

use crate::index::Index;
use crate::lock;
use crate::manifest::Manifest;
use crate::symfile::SymBuilder;

pub const SYMBOLS_LOCK: &str = "symbols.lock";

#[derive(Clone, Debug, Default)]
pub struct SymbolizeStats {
    pub segments: usize,
    pub files: u64,
    pub symbols: u64,
    pub bytes: u64,
    /// Tree-sitter CPU time summed over threads.
    pub cpu: Duration,
    pub time: Duration,
}

/// True when the index wants symbols and some segment has none yet.
pub fn needed(dir: &Path) -> bool {
    Manifest::load(dir)
        .map(|m| m.symbols && m.segments.iter().any(|s| !s.syms))
        .unwrap_or(false)
}

/// Fills in missing sidecars. Returns `None` if another process holds the
/// symbols lock. `cancel` is checked between chunks of files.
pub fn backfill(
    root: &Path,
    dir: &Path,
    threads: usize,
    progress: bool,
    cancel: &AtomicBool,
) -> Result<Option<SymbolizeStats>> {
    let Some(_guard) = lock::try_lock(&dir.join(SYMBOLS_LOCK))? else {
        return Ok(None);
    };
    let t0 = Instant::now();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads.max(1))
        .build()?;
    let mut stats = SymbolizeStats::default();
    let cpu = AtomicU64::new(0);
    // A compaction can replace a segment while we work on it; retry a few
    // times with the new layout.
    for _round in 0..8 {
        let index = Index::open(dir)?;
        if !index.manifest.symbols {
            break;
        }
        let todo: Vec<usize> = (0..index.segments.len())
            .filter(|&i| index.segments[i].syms.is_none())
            .collect();
        if todo.is_empty() {
            break;
        }
        for si in todo {
            if cancel.load(Ordering::Relaxed) {
                return Ok(Some(stats));
            }
            let s = &index.segments[si];
            let name = s.info.name.clone();
            let n = s.seg.num_files();
            let mut sb = SymBuilder::new();
            let ids: Vec<u32> = (0..n).filter(|&id| s.is_live(id)).collect();
            let mut done = 0usize;
            for chunk in ids.chunks(1024) {
                if cancel.load(Ordering::Relaxed) {
                    return Ok(Some(stats));
                }
                let results: Vec<(u32, Vec<scout_symbols::Sym>)> = pool.install(|| {
                    chunk
                        .par_iter()
                        .map(|&id| {
                            let rel = s.seg.path(id);
                            let Some(lang) = scout_symbols::Lang::detect(rel, b"") else {
                                return (id, Vec::new());
                            };
                            // Re-detect with content only for .h files.
                            let abs = root.join(String::from_utf8_lossy(rel).as_ref());
                            let Ok(data) = fs::read(&abs) else {
                                return (id, Vec::new());
                            };
                            let lang = if rel.ends_with(b".h") {
                                scout_symbols::Lang::detect(rel, &data).unwrap_or(lang)
                            } else {
                                lang
                            };
                            let t = Instant::now();
                            let syms = scout_symbols::extract(lang, &data);
                            cpu.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
                            (id, syms)
                        })
                        .collect()
                });
                for (id, syms) in &results {
                    stats.symbols += syms.len() as u64;
                    sb.add(*id, syms);
                }
                done += chunk.len();
                if progress && (done / 1024) % 16 == 0 && ids.len() > 4096 {
                    eprintln!("scout: symbols {}/{} files", done, ids.len());
                }
            }
            stats.files += ids.len() as u64;
            // Written under a name gc ignores, renamed under the writer lock.
            let tmp = dir.join(format!("symtmp-{}.sym", name.trim_end_matches(".idx")));
            sb.write(&tmp)?;
            let _w = lock::lock(&dir.join(lock::WRITE_LOCK))?;
            let mut m = Manifest::load(dir)?;
            match m.segments.iter_mut().find(|x| x.name == name) {
                Some(info) if !info.syms => {
                    let final_name = info.sym_name();
                    stats.bytes += fs::metadata(&tmp)?.len();
                    fs::rename(&tmp, dir.join(&final_name))
                        .with_context(|| format!("installing {}", final_name))?;
                    info.syms = true;
                    m.generation += 1;
                    m.store(dir)?;
                    stats.segments += 1;
                }
                _ => {
                    let _ = fs::remove_file(&tmp);
                }
            }
        }
    }
    stats.cpu = Duration::from_nanos(cpu.load(Ordering::Relaxed));
    stats.time = t0.elapsed();
    Ok(Some(stats))
}
