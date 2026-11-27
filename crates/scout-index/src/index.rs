//! A read snapshot of an index, plus the incremental update path used by
//! the daemon.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use roaring::RoaringBitmap;

use crate::build::{read_and_extract, Extracted};
use crate::filter;
use crate::manifest::{self, Manifest, SegmentInfo};
use crate::rules::Rules;
use crate::segment::{FileMeta, Segment, SegmentWriter};
use crate::symfile::{sym_path, SymBuilder, SymFile};
use crate::trigram::Extractor;
use scout_symbols::Sym;

pub struct LoadedSegment {
    pub info: SegmentInfo,
    pub seg: Segment,
    pub del: RoaringBitmap,
    pub syms: Option<SymFile>,
}

impl LoadedSegment {
    #[inline]
    pub fn is_live(&self, id: u32) -> bool {
        !self.del.contains(id)
    }
}

/// A consistent view of the index as named by one manifest.
pub struct Index {
    pub dir: PathBuf,
    pub manifest: Manifest,
    pub segments: Vec<LoadedSegment>,
}

/// One live (not tombstoned) file.
pub struct LiveFile<'a> {
    pub seg: usize,
    pub id: u32,
    pub path: &'a [u8],
    pub meta: FileMeta,
}

pub fn load_del(dir: &Path, name: &str) -> Result<RoaringBitmap> {
    let p = dir.join(name);
    let data = fs::read(&p).with_context(|| format!("reading {}", p.display()))?;
    Ok(RoaringBitmap::deserialize_from(&data[..])?)
}

fn store_del(dir: &Path, name: &str, bm: &RoaringBitmap) -> Result<()> {
    let tmp = dir.join(format!("{}.tmp", name));
    {
        let mut f = fs::File::create(&tmp)?;
        let mut buf = Vec::with_capacity(bm.serialized_size());
        bm.serialize_into(&mut buf)?;
        f.write_all(&buf)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, dir.join(name))?;
    Ok(())
}

impl Index {
    pub fn open(dir: &Path) -> Result<Index> {
        let manifest = Manifest::load(dir)?;
        let mut segments = Vec::with_capacity(manifest.segments.len());
        for info in &manifest.segments {
            let seg = Segment::open(&dir.join(&info.name))?;
            let del = match &info.del {
                Some(d) => load_del(dir, d)?,
                None => RoaringBitmap::new(),
            };
            let syms = if info.syms {
                Some(SymFile::open(&dir.join(info.sym_name()))?)
            } else {
                None
            };
            segments.push(LoadedSegment {
                info: info.clone(),
                seg,
                del,
                syms,
            });
        }
        Ok(Index {
            dir: dir.to_path_buf(),
            manifest,
            segments,
        })
    }

    /// Finds the live copy of `rel`, searching newest segments first.
    pub fn lookup(&self, rel: &[u8]) -> Option<(usize, u32)> {
        for (si, s) in self.segments.iter().enumerate().rev() {
            if let Some(id) = s.seg.find_path(rel) {
                if s.is_live(id) {
                    return Some((si, id));
                }
            }
        }
        None
    }

    pub fn live_files(&self) -> impl Iterator<Item = LiveFile<'_>> {
        self.segments.iter().enumerate().flat_map(|(si, s)| {
            (0..s.seg.num_files())
                .filter(move |&id| s.is_live(id))
                .map(move |id| LiveFile {
                    seg: si,
                    id,
                    path: s.seg.path(id),
                    meta: s.seg.meta(id),
                })
        })
    }

    pub fn live_count(&self) -> u64 {
        self.segments
            .iter()
            .map(|s| s.seg.num_files() as u64 - s.del.len())
            .sum()
    }

    pub fn disk_bytes(&self) -> u64 {
        self.segments
            .iter()
            .map(|s| {
                s.seg.byte_len() as u64 + s.syms.as_ref().map(|y| y.byte_len() as u64).unwrap_or(0)
            })
            .sum()
    }

    /// Whether every segment has its symbols (false if the index has none).
    pub fn symbols_ready(&self) -> bool {
        self.manifest.symbols && self.segments.iter().all(|s| s.syms.is_some())
    }
}

/// What one incremental update did.
#[derive(Debug, Default, Clone)]
pub struct ApplyStats {
    pub examined: usize,
    pub unchanged: usize,
    pub indexed: usize,
    pub removed: usize,
    pub segment: Option<String>,
}

/// Re-examines `paths` (relative to the root) and records the result as a
/// new delta segment plus tombstones, then swaps the manifest. Paths that
/// still match their indexed size and mtime are skipped without reading.
pub fn apply_changes(
    dir: &Path,
    rules: &Rules,
    paths: &[PathBuf],
    max_file_size: u64,
    commit: Option<String>,
) -> Result<ApplyStats> {
    let index = Index::open(dir)?;
    let mut manifest = index.manifest.clone();
    let root = rules.root().to_path_buf();
    let mut stats = ApplyStats::default();
    let symbols = manifest.symbols;
    let mut new_files: Vec<NewFile> = Vec::new();
    let mut tomb: Vec<RoaringBitmap> = index.segments.iter().map(|s| s.del.clone()).collect();
    let mut touched = vec![false; index.segments.len()];
    let mut ex = Extractor::new();

    let mut paths: Vec<PathBuf> = paths.to_vec();
    paths.sort();
    paths.dedup();
    for rel in &paths {
        stats.examined += 1;
        let rel_bytes = rel.as_os_str().as_encoded_bytes().to_vec();
        let current = index.lookup(&rel_bytes);
        let abs = root.join(rel);
        let md = fs::symlink_metadata(&abs).ok().filter(|m| m.is_file());
        let mut keep: Option<(FileMeta, Vec<u32>, Vec<Sym>)> = None;
        if let Some(md) = &md {
            if rules.check(rel, false).is_included()
                && filter::check_size(md.len(), max_file_size).is_none()
            {
                if let Some((si, id)) = current {
                    let m = index.segments[si].seg.meta(id);
                    if m.size == md.len() && m.mtime_ns == crate::segment::mtime_ns(md) {
                        stats.unchanged += 1;
                        continue;
                    }
                }
                if let Ok(Extracted::Indexed {
                    meta,
                    trigrams,
                    syms,
                    ..
                }) = read_and_extract(&abs, max_file_size, &mut ex, symbols)
                {
                    if let Some((si, id)) = current {
                        if index.segments[si].seg.meta(id).hash == meta.hash {
                            // Content is identical (touched or reverted): keep the
                            // old copy rather than churn a new segment.
                            stats.unchanged += 1;
                            continue;
                        }
                    }
                    keep = Some((meta, trigrams, syms));
                }
            }
        }
        // Tombstone every older live copy.
        for (si, s) in index.segments.iter().enumerate() {
            if let Some(id) = s.seg.find_path(&rel_bytes) {
                if tomb[si].insert(id) {
                    touched[si] = true;
                }
            }
        }
        match keep {
            Some((meta, tris, syms)) => {
                stats.indexed += 1;
                new_files.push(NewFile {
                    path: rel_bytes,
                    meta,
                    trigrams: tris,
                    syms,
                });
            }
            None => {
                if current.is_some() {
                    stats.removed += 1;
                }
            }
        }
    }

    if new_files.is_empty() && !touched.iter().any(|&t| t) {
        if commit.is_some() && commit != manifest.commit {
            manifest.commit = commit;
            manifest.generation += 1;
            manifest.store(dir)?;
        }
        return Ok(stats);
    }

    manifest.generation += 1;
    let gen = manifest.generation;
    if !new_files.is_empty() {
        let name = manifest.alloc_segment_name();
        write_small_segment(&dir.join(&name), &new_files, symbols)?;
        manifest.segments.push(SegmentInfo::new(
            name.clone(),
            new_files.len() as u32,
            "delta",
            symbols,
        ));
        stats.segment = Some(name);
    }
    for (si, t) in touched.iter().enumerate() {
        if *t {
            let base = index.segments[si]
                .info
                .name
                .trim_end_matches(".idx")
                .to_string();
            let name = format!("{}.del-{:06}", base, gen);
            store_del(dir, &name, &tomb[si])?;
            manifest.segments[si].del = Some(name);
            manifest.segments[si].deleted = tomb[si].len() as u32;
        }
    }
    // Drop segments that are now entirely dead.
    manifest.segments.retain(|s| s.deleted < s.files);
    if commit.is_some() {
        manifest.commit = commit;
    }
    manifest.updated_at = manifest::now_secs();
    manifest.store(dir)?;
    manifest::gc(dir, &manifest);
    Ok(stats)
}

/// One file to put in a small segment.
pub struct NewFile {
    pub path: Vec<u8>,
    pub meta: FileMeta,
    pub trigrams: Vec<u32>,
    pub syms: Vec<Sym>,
}

/// Writes a segment from an in-memory list of files (sorted by path here),
/// plus its symbol sidecar when `symbols` is set.
pub fn write_small_segment(path: &Path, files: &[NewFile], symbols: bool) -> Result<()> {
    let mut order: Vec<usize> = (0..files.len()).collect();
    order.sort_by(|&a, &b| files[a].path.cmp(&files[b].path));
    let mut pairs: Vec<u64> = Vec::new();
    let mut sb = SymBuilder::new();
    let mut w = SegmentWriter::create(path)?;
    for (new_id, &i) in order.iter().enumerate() {
        let f = &files[i];
        w.add_file(&f.path, f.meta);
        pairs.extend(
            f.trigrams
                .iter()
                .map(|&t| ((t as u64) << 32) | new_id as u64),
        );
        if symbols {
            sb.add(new_id as u32, &f.syms);
        }
    }
    if symbols {
        sb.write(&sym_path(path))?;
    }
    pairs.sort_unstable();
    let mut ids = Vec::new();
    let mut i = 0;
    while i < pairs.len() {
        let tri = (pairs[i] >> 32) as u32;
        ids.clear();
        while i < pairs.len() && (pairs[i] >> 32) as u32 == tri {
            ids.push(pairs[i] as u32);
            i += 1;
        }
        w.add_postings(tri, &ids)?;
    }
    w.finish()
}
