//! Full index builds.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rayon::prelude::*;

use crate::filter::{self, Skip};
use crate::lock;
use crate::manifest::{self, Manifest, SegmentInfo};
use crate::merge::{merge, merge_has_syms, MergeInput};
use crate::rules::Rules;
use crate::segment::{FileMeta, Segment, SegmentWriter};
use crate::symfile::{sym_path, SymBuilder, SymFile};
use crate::trigram::{Extractor, NUM_TRIGRAMS};
use scout_symbols::Sym;

#[derive(Clone, Debug)]
pub struct BuildOptions {
    pub threads: usize,
    /// Memory budget for in-flight postings, in bytes.
    pub mem_budget: usize,
    pub max_file_size: u64,
    pub excludes: Vec<String>,
    pub commit: Option<String>,
    /// Print progress to stderr.
    pub progress: bool,
    /// Index definitions and calls with tree-sitter. Unless
    /// `inline_symbols` is set, the build only marks the index as wanting
    /// symbols and `symbolize::backfill` adds them afterwards, so text search
    /// is ready several times sooner.
    pub symbols: bool,
    pub inline_symbols: bool,
}

impl Default for BuildOptions {
    fn default() -> Self {
        BuildOptions {
            threads: default_threads(),
            mem_budget: 512 << 20,
            max_file_size: filter::DEFAULT_MAX_FILE_SIZE,
            excludes: Vec::new(),
            commit: None,
            progress: false,
            symbols: true,
            inline_symbols: false,
        }
    }
}

/// Half the cores, at least one.
pub fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .div_ceil(2)
        .max(1)
}

#[derive(Clone, Debug, Default)]
pub struct BuildStats {
    pub files_walked: u64,
    pub files_indexed: u64,
    pub bytes_indexed: u64,
    pub skipped_large: u64,
    pub skipped_binary: u64,
    pub skipped_minified: u64,
    pub skipped_unreadable: u64,
    pub batches: u64,
    pub index_bytes: u64,
    pub trigrams: u64,
    /// Symbol records (definitions plus calls) and sidecar size.
    pub symbols: u64,
    pub sym_bytes: u64,
    /// CPU time spent parsing and querying with tree-sitter, all threads.
    pub sym_cpu: Duration,
    pub walk_time: Duration,
    pub total_time: Duration,
}

pub enum Extracted {
    Indexed {
        meta: FileMeta,
        trigrams: Vec<u32>,
        bytes: u64,
        syms: Vec<Sym>,
        sym_ns: u64,
    },
    Skipped(Skip),
}

/// Reads one file and extracts its trigrams (and symbols if asked),
/// applying the content rules.
pub fn read_and_extract(
    abs: &Path,
    max_size: u64,
    ex: &mut Extractor,
    symbols: bool,
) -> Result<Extracted> {
    let md = fs::symlink_metadata(abs)?;
    if let Some(s) = filter::check_size(md.len(), max_size) {
        return Ok(Extracted::Skipped(s));
    }
    let data = fs::read(abs)?;
    if let Some(s) = filter::check_size(data.len() as u64, max_size) {
        return Ok(Extracted::Skipped(s));
    }
    if let Some(s) = filter::check_content(&data) {
        return Ok(Extracted::Skipped(s));
    }
    let hash = xxhash_rust::xxh3::xxh3_64(&data);
    let mut meta = FileMeta::from_metadata(&md, hash);
    meta.size = data.len() as u64;
    let mut trigrams = Vec::new();
    ex.extract(&data, &mut trigrams);
    let (syms, sym_ns) = if symbols {
        let t = Instant::now();
        let s = scout_symbols::extract_path(abs.as_os_str().as_encoded_bytes(), &data);
        (s, t.elapsed().as_nanos() as u64)
    } else {
        (Vec::new(), 0)
    };
    Ok(Extracted::Indexed {
        meta,
        trigrams,
        bytes: data.len() as u64,
        syms,
        sym_ns,
    })
}

thread_local! {
    static EXTRACTOR: RefCell<Option<Extractor>> = const { RefCell::new(None) };
}

fn with_extractor<R>(f: impl FnOnce(&mut Extractor) -> R) -> R {
    EXTRACTOR.with(|cell| {
        let mut b = cell.borrow_mut();
        f(b.get_or_insert_with(Extractor::new))
    })
}

/// Walks the tree and returns sorted relative paths of candidate files.
pub fn walk(
    rules: &Rules,
    threads: usize,
    max_size: u64,
    skipped_large: &AtomicU64,
) -> Vec<PathBuf> {
    let (tx, rx) = mpsc::channel::<Vec<PathBuf>>();
    let root = rules.root().to_path_buf();
    rules.walker(threads).build_parallel().run(|| {
        let tx = tx.clone();
        let root = root.clone();
        let mut buf: Vec<PathBuf> = Vec::new();
        struct Flush(mpsc::Sender<Vec<PathBuf>>, Vec<PathBuf>);
        impl Drop for Flush {
            fn drop(&mut self) {
                let _ = self.0.send(std::mem::take(&mut self.1));
            }
        }
        let mut guard = Flush(tx, std::mem::take(&mut buf));
        Box::new(move |entry| {
            if let Ok(e) = entry {
                if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    let big = e.metadata().map(|m| m.len() > max_size).unwrap_or(false);
                    if big {
                        skipped_large.fetch_add(1, Ordering::Relaxed);
                    } else if let Ok(rel) = e.path().strip_prefix(&root) {
                        guard.1.push(rel.to_path_buf());
                    }
                }
            }
            ignore::WalkState::Continue
        })
    });
    drop(tx);
    let mut all: Vec<PathBuf> = rx.into_iter().flatten().collect();
    all.par_sort_unstable_by(|a, b| {
        a.as_os_str()
            .as_encoded_bytes()
            .cmp(b.as_os_str().as_encoded_bytes())
    });
    all
}

/// In-memory batch of files waiting to become a segment.
struct Batch {
    files: Vec<(Vec<u8>, FileMeta)>,
    tris: Vec<Vec<u32>>,
    pairs: usize,
    syms: Option<SymBuilder>,
}

impl Batch {
    fn new(symbols: bool) -> Batch {
        Batch {
            files: Vec::new(),
            tris: Vec::new(),
            pairs: 0,
            syms: symbols.then(SymBuilder::new),
        }
    }

    /// Memory in units of 8-byte postings pairs.
    fn weight(&self) -> usize {
        self.pairs
            + self
                .syms
                .as_ref()
                .map(|s| s.approx_bytes() / 8)
                .unwrap_or(0)
    }
}

/// Writes a batch with a counting sort over the trigram space; `counts` is
/// a reusable 2^24 scratch array that is left zeroed.
fn flush_batch(batch: &mut Batch, path: &Path, counts: &mut [u32]) -> Result<()> {
    let mut w = SegmentWriter::create(path)?;
    for (p, m) in &batch.files {
        w.add_file(p, *m);
    }
    for tris in &batch.tris {
        for &t in tris {
            counts[t as usize] += 1;
        }
    }
    // Exclusive prefix sums: counts[t] becomes the start of bucket t.
    let mut sum = 0u32;
    for c in counts.iter_mut() {
        let n = *c;
        *c = sum;
        sum += n;
    }
    let mut ids = vec![0u32; sum as usize];
    for (id, tris) in batch.tris.iter().enumerate() {
        for &t in tris {
            let slot = &mut counts[t as usize];
            ids[*slot as usize] = id as u32;
            *slot += 1;
        }
    }
    // counts[t] is now the end of bucket t.
    let mut start = 0usize;
    for (t, c) in counts.iter_mut().enumerate() {
        let end = *c as usize;
        if end > start {
            w.add_postings(t as u32, &ids[start..end])?;
        }
        start = end;
        *c = 0;
    }
    w.finish()?;
    if let Some(sb) = batch.syms.as_mut() {
        sb.write(&sym_path(path))?;
        sb.clear();
    }
    batch.files.clear();
    batch.tris.clear();
    batch.pairs = 0;
    Ok(())
}

/// Builds a fresh index for `root` into `dir`, replacing whatever was there.
pub fn build_full(root: &Path, dir: &Path, opts: &BuildOptions) -> Result<BuildStats> {
    let t0 = Instant::now();
    manifest::ensure_dir(dir)?;
    let rules = Rules::new(root, &opts.excludes)?;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(opts.threads.max(1))
        .build()
        .context("building thread pool")?;
    let mut stats = BuildStats::default();

    let skipped_large = AtomicU64::new(0);
    let paths = walk(
        &rules,
        opts.threads.max(1),
        opts.max_file_size,
        &skipped_large,
    );
    stats.walk_time = t0.elapsed();
    stats.files_walked = paths.len() as u64 + skipped_large.load(Ordering::Relaxed);
    stats.skipped_large = skipped_large.load(Ordering::Relaxed);
    if opts.progress {
        eprintln!(
            "scout: walked {} files in {:.2?}",
            stats.files_walked, stats.walk_time
        );
    }

    let scratch_bytes = NUM_TRIGRAMS * 4;
    let pair_limit = (opts.mem_budget.saturating_sub(scratch_bytes) / 8).max(1 << 20);
    let mut counts = vec![0u32; NUM_TRIGRAMS];
    let tmp_prefix = format!("build-{}", std::process::id());
    let mut temp_segments: Vec<PathBuf> = Vec::new();
    let inline = opts.symbols && opts.inline_symbols;
    let mut batch = Batch::new(inline);
    let mut sym_ns = 0u64;
    let done = Arc::new(AtomicU64::new(0));

    for chunk in paths.chunks(2048) {
        let results: Vec<(usize, Result<Extracted>)> = pool.install(|| {
            chunk
                .par_iter()
                .enumerate()
                .map(|(i, rel)| {
                    let abs = root.join(rel);
                    (
                        i,
                        with_extractor(|ex| read_and_extract(&abs, opts.max_file_size, ex, inline)),
                    )
                })
                .collect()
        });
        for (i, r) in results {
            match r {
                Ok(Extracted::Indexed {
                    meta,
                    trigrams,
                    bytes,
                    syms,
                    sym_ns: ns,
                }) => {
                    stats.files_indexed += 1;
                    sym_ns += ns;
                    stats.symbols += syms.len() as u64;
                    if let Some(sb) = batch.syms.as_mut() {
                        sb.add(batch.files.len() as u32, &syms);
                    }
                    stats.bytes_indexed += bytes;
                    batch.pairs += trigrams.len();
                    batch
                        .files
                        .push((chunk[i].as_os_str().as_encoded_bytes().to_vec(), meta));
                    batch.tris.push(trigrams);
                }
                Ok(Extracted::Skipped(Skip::TooLarge)) => stats.skipped_large += 1,
                Ok(Extracted::Skipped(Skip::Binary)) => stats.skipped_binary += 1,
                Ok(Extracted::Skipped(Skip::Minified)) => stats.skipped_minified += 1,
                Err(_) => stats.skipped_unreadable += 1,
            }
        }
        let n = done.fetch_add(chunk.len() as u64, Ordering::Relaxed) + chunk.len() as u64;
        if batch.weight() >= pair_limit {
            let p = dir.join(format!("{}-{}.idx", tmp_prefix, temp_segments.len()));
            flush_batch(&mut batch, &p, &mut counts)?;
            temp_segments.push(p);
            if opts.progress {
                eprintln!(
                    "scout: {}/{} files, {} batches",
                    n,
                    paths.len(),
                    temp_segments.len()
                );
            }
        }
    }
    if !batch.files.is_empty() || temp_segments.is_empty() {
        let p = dir.join(format!("{}-{}.idx", tmp_prefix, temp_segments.len()));
        flush_batch(&mut batch, &p, &mut counts)?;
        temp_segments.push(p);
    }
    drop(counts);
    stats.sym_cpu = Duration::from_nanos(sym_ns);
    stats.batches = temp_segments.len() as u64;

    // Swap in under the writer lock so a running daemon can't interleave.
    let _guard = lock::lock(&dir.join(lock::WRITE_LOCK))?;
    let mut m = match Manifest::load(dir) {
        Ok(old) => Manifest {
            segments: Vec::new(),
            ..old
        },
        Err(_) => Manifest::new(root, opts.excludes.clone()),
    };
    m.root = root.to_string_lossy().into_owned();
    m.excludes = opts.excludes.clone();
    let name = m.alloc_segment_name();
    let final_path = dir.join(&name);
    let nfiles = if temp_segments.len() == 1 {
        fs::rename(&temp_segments[0], &final_path)?;
        if inline {
            fs::rename(sym_path(&temp_segments[0]), sym_path(&final_path))?;
        }
        stats.files_indexed as u32
    } else {
        if opts.progress {
            eprintln!("scout: merging {} batches", temp_segments.len());
        }
        let segs: Vec<Segment> = temp_segments
            .iter()
            .map(|p| Segment::open(p))
            .collect::<Result<_>>()?;
        let syms: Vec<Option<SymFile>> = temp_segments
            .iter()
            .map(|p| {
                if inline {
                    SymFile::open(&sym_path(p)).map(Some)
                } else {
                    Ok(None)
                }
            })
            .collect::<Result<_>>()?;
        let inputs: Vec<MergeInput> = segs
            .iter()
            .zip(&syms)
            .map(|(s, y)| MergeInput {
                seg: s,
                del: None,
                syms: y.as_ref(),
            })
            .collect();
        debug_assert_eq!(merge_has_syms(&inputs), inline);
        let n = merge(&inputs, &final_path)?;
        drop(inputs);
        drop(segs);
        drop(syms);
        for p in &temp_segments {
            let _ = fs::remove_file(p);
            let _ = fs::remove_file(sym_path(p));
        }
        n
    };
    let seg = Segment::open(&final_path)?;
    stats.index_bytes = seg.byte_len() as u64;
    stats.trigrams = seg.num_trigrams() as u64;
    if inline {
        stats.sym_bytes = fs::metadata(sym_path(&final_path))?.len();
    }
    m.segments = vec![SegmentInfo::new(name, nfiles, "base", inline)];
    m.symbols = opts.symbols;
    m.generation += 1;
    m.commit = opts.commit.clone();
    m.built_at = manifest::now_secs();
    m.updated_at = m.built_at;
    m.store(dir)?;
    manifest::gc(dir, &m);
    stats.total_time = t0.elapsed();
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::Index;

    #[test]
    fn build_batches_and_merge() {
        let src = tempfile::tempdir().unwrap();
        let root = src.path().canonicalize().unwrap();
        for i in 0..300 {
            let p = root.join(format!("d{}/f{}.txt", i % 7, i));
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, format!("hello world {}\nunique_token_{}\n", i, i)).unwrap();
        }
        fs::write(root.join("bin.dat"), b"ab\0cd").unwrap();
        let idx = tempfile::tempdir().unwrap();
        // A tiny budget forces many batches and a merge.
        let opts = BuildOptions {
            threads: 2,
            mem_budget: 0,
            ..Default::default()
        };
        // pair_limit floors at 1M pairs; shrink further via many files is
        // impractical here, so check the single-batch path and then the
        // merge path directly.
        let stats = build_full(&root, idx.path(), &opts).unwrap();
        assert_eq!(stats.files_indexed, 300);
        assert_eq!(stats.skipped_binary, 1);
        let index = Index::open(idx.path()).unwrap();
        assert_eq!(index.live_count(), 300);
        let (si, id) = index.lookup(b"d3/f10.txt").unwrap();
        let mut v = Vec::new();
        index.segments[si]
            .seg
            .postings(crate::trigram::pack(b'h', b'e', b'l'), &mut v);
        assert_eq!(v.len(), 300);
        assert!(v.contains(&id));
    }

    #[test]
    fn flush_and_merge_many_batches() {
        let idx = tempfile::tempdir().unwrap();
        let mut counts = vec![0u32; NUM_TRIGRAMS];
        let mut segs = Vec::new();
        for b in 0..3u32 {
            let mut batch = Batch::new(false);
            for f in 0..5u32 {
                let n = b * 5 + f;
                batch
                    .files
                    .push((format!("p{:03}", n).into_bytes(), FileMeta::default()));
                batch.tris.push(vec![1, 100 + n]);
            }
            let p = idx.path().join(format!("t{}.idx", b));
            flush_batch(&mut batch, &p, &mut counts).unwrap();
            segs.push(Segment::open(&p).unwrap());
        }
        assert!(counts.iter().all(|&c| c == 0));
        let inputs: Vec<MergeInput> = segs
            .iter()
            .map(|s| MergeInput {
                seg: s,
                del: None,
                syms: None,
            })
            .collect();
        let out = idx.path().join("m.idx");
        assert_eq!(merge(&inputs, &out).unwrap(), 15);
        let m = Segment::open(&out).unwrap();
        let mut v = Vec::new();
        m.postings(1, &mut v);
        assert_eq!(v, (0..15).collect::<Vec<_>>());
        v.clear();
        m.postings(107, &mut v);
        assert_eq!(v, vec![7]);
        assert_eq!(m.path(7), b"p007");
    }
}
