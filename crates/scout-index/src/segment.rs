//! Immutable segment files.
//!
//! Layout (all integers little endian):
//!
//! ```text
//! header   64 bytes: magic "SCOUTSG1", version u32, flags u32, num_files u32,
//!          num_trigrams u32, paths_off u64, pathidx_off u64, meta_off u64,
//!          dict_off u64, postings_off u64
//! paths    concatenated relative paths, sorted bytewise
//! pathidx  (num_files + 1) u32 offsets into paths
//! meta     num_files * 24 bytes: size u64, mtime_ns i64, xxh3 u64
//! postings delta-varint file ids, one run per trigram
//! dict     num_trigrams * 16 bytes: trigram u32, doc count u32, offset u64
//!          (offset is relative to postings_off), sorted by trigram
//! ```
//!
//! File ids are local to a segment and follow path order, so a path lookup
//! is a binary search.

use std::fs::{self, File};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use memmap2::Mmap;

use crate::varint;

pub const MAGIC: &[u8; 8] = b"SCOUTSG1";
pub const VERSION: u32 = 1;
const HEADER_LEN: usize = 64;
const META_LEN: usize = 24;
const DICT_LEN: usize = 16;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FileMeta {
    pub size: u64,
    pub mtime_ns: i64,
    pub hash: u64,
}

impl FileMeta {
    pub fn from_metadata(md: &fs::Metadata, hash: u64) -> FileMeta {
        FileMeta {
            size: md.len(),
            mtime_ns: mtime_ns(md),
            hash,
        }
    }
}

pub fn mtime_ns(md: &fs::Metadata) -> i64 {
    match md.modified() {
        Ok(t) => match t.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d.as_nanos() as i64,
            Err(e) => -(e.duration().as_nanos() as i64),
        },
        Err(_) => 0,
    }
}

/// Streams a segment to disk. Call [`SegmentWriter::add_file`] for every
/// file in path order, then [`SegmentWriter::add_postings`] for every
/// trigram in ascending order, then [`SegmentWriter::finish`].
pub struct SegmentWriter {
    out: BufWriter<File>,
    tmp_path: std::path::PathBuf,
    final_path: std::path::PathBuf,
    paths: Vec<u8>,
    path_idx: Vec<u32>,
    metas: Vec<FileMeta>,
    files_written: bool,
    postings_off: u64,
    pos: u64,
    dict: Vec<(u32, u32, u64)>,
    buf: Vec<u8>,
    last_tri: Option<u32>,
}

impl SegmentWriter {
    pub fn create(final_path: &Path) -> Result<SegmentWriter> {
        let tmp_path = final_path.with_extension("tmp");
        let file =
            File::create(&tmp_path).with_context(|| format!("creating {}", tmp_path.display()))?;
        let mut out = BufWriter::with_capacity(1 << 20, file);
        out.write_all(&[0u8; HEADER_LEN])?;
        Ok(SegmentWriter {
            out,
            tmp_path,
            final_path: final_path.to_path_buf(),
            paths: Vec::new(),
            path_idx: vec![0],
            metas: Vec::new(),
            files_written: false,
            postings_off: 0,
            pos: HEADER_LEN as u64,
            dict: Vec::new(),
            buf: Vec::new(),
            last_tri: None,
        })
    }

    pub fn add_file(&mut self, path: &[u8], meta: FileMeta) {
        assert!(!self.files_written, "add_file after postings");
        self.paths.extend_from_slice(path);
        self.path_idx.push(self.paths.len() as u32);
        self.metas.push(meta);
    }

    pub fn num_files(&self) -> u32 {
        self.metas.len() as u32
    }

    fn write_files(&mut self) -> Result<()> {
        if self.files_written {
            return Ok(());
        }
        self.files_written = true;
        self.out.write_all(&self.paths)?;
        self.pos += self.paths.len() as u64;
        self.pad8()?;
        for off in &self.path_idx {
            self.out.write_all(&off.to_le_bytes())?;
        }
        self.pos += 4 * self.path_idx.len() as u64;
        self.pad8()?;
        for m in &self.metas {
            self.out.write_all(&m.size.to_le_bytes())?;
            self.out.write_all(&m.mtime_ns.to_le_bytes())?;
            self.out.write_all(&m.hash.to_le_bytes())?;
        }
        self.pos += (META_LEN * self.metas.len()) as u64;
        self.pad8()?;
        self.postings_off = self.pos;
        Ok(())
    }

    fn pad8(&mut self) -> Result<()> {
        while self.pos % 8 != 0 {
            self.out.write_all(&[0])?;
            self.pos += 1;
        }
        Ok(())
    }

    /// Adds the posting list for `tri`. `ids` must be strictly increasing.
    pub fn add_postings(&mut self, tri: u32, ids: &[u32]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        self.write_files()?;
        if let Some(last) = self.last_tri {
            assert!(tri > last, "trigrams out of order");
        }
        self.last_tri = Some(tri);
        self.buf.clear();
        varint::encode_postings(ids, &mut self.buf);
        self.dict
            .push((tri, ids.len() as u32, self.pos - self.postings_off));
        self.out.write_all(&self.buf)?;
        self.pos += self.buf.len() as u64;
        Ok(())
    }

    pub fn finish(mut self) -> Result<()> {
        self.write_files()?;
        self.pad8()?;
        let dict_off = self.pos;
        for &(t, c, o) in &self.dict {
            self.out.write_all(&t.to_le_bytes())?;
            self.out.write_all(&c.to_le_bytes())?;
            self.out.write_all(&o.to_le_bytes())?;
        }
        let num_files = self.metas.len() as u32;
        let paths_off = HEADER_LEN as u64;
        let pathidx_off = align8(paths_off + self.paths.len() as u64);
        let meta_off = align8(pathidx_off + 4 * self.path_idx.len() as u64);
        let mut h = Vec::with_capacity(HEADER_LEN);
        h.extend_from_slice(MAGIC);
        h.extend_from_slice(&VERSION.to_le_bytes());
        h.extend_from_slice(&0u32.to_le_bytes());
        h.extend_from_slice(&num_files.to_le_bytes());
        h.extend_from_slice(&(self.dict.len() as u32).to_le_bytes());
        for v in [
            paths_off,
            pathidx_off,
            meta_off,
            dict_off,
            self.postings_off,
        ] {
            h.extend_from_slice(&v.to_le_bytes());
        }
        debug_assert_eq!(h.len(), HEADER_LEN);
        let mut file = self.out.into_inner().map_err(|e| e.into_error())?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&h)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&self.tmp_path, &self.final_path)?;
        Ok(())
    }
}

fn align8(v: u64) -> u64 {
    (v + 7) & !7
}

/// A memory-mapped, read-only segment.
pub struct Segment {
    mmap: Mmap,
    num_files: u32,
    num_trigrams: u32,
    paths_off: usize,
    pathidx_off: usize,
    meta_off: usize,
    dict_off: usize,
    postings_off: usize,
}

#[inline]
fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}
#[inline]
fn u64_at(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}

impl Segment {
    pub fn open(path: &Path) -> Result<Segment> {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        // SAFETY: segments are immutable once renamed into place; writers
        // never modify a published segment.
        let mmap = unsafe { Mmap::map(&file)? };
        if mmap.len() < HEADER_LEN || &mmap[0..8] != MAGIC {
            bail!("{} is not a scout segment", path.display());
        }
        let version = u32_at(&mmap, 8);
        if version != VERSION {
            bail!(
                "{}: unsupported segment version {}",
                path.display(),
                version
            );
        }
        let seg = Segment {
            num_files: u32_at(&mmap, 16),
            num_trigrams: u32_at(&mmap, 20),
            paths_off: u64_at(&mmap, 24) as usize,
            pathidx_off: u64_at(&mmap, 32) as usize,
            meta_off: u64_at(&mmap, 40) as usize,
            dict_off: u64_at(&mmap, 48) as usize,
            postings_off: u64_at(&mmap, 56) as usize,
            mmap,
        };
        let need = seg.dict_off + DICT_LEN * seg.num_trigrams as usize;
        if need > seg.mmap.len() {
            bail!("{}: truncated segment", path.display());
        }
        Ok(seg)
    }

    pub fn num_files(&self) -> u32 {
        self.num_files
    }

    pub fn num_trigrams(&self) -> u32 {
        self.num_trigrams
    }

    pub fn byte_len(&self) -> usize {
        self.mmap.len()
    }

    pub fn path(&self, id: u32) -> &[u8] {
        let i = self.pathidx_off + 4 * id as usize;
        let s = u32_at(&self.mmap, i) as usize;
        let e = u32_at(&self.mmap, i + 4) as usize;
        &self.mmap[self.paths_off + s..self.paths_off + e]
    }

    pub fn meta(&self, id: u32) -> FileMeta {
        let o = self.meta_off + META_LEN * id as usize;
        FileMeta {
            size: u64_at(&self.mmap, o),
            mtime_ns: u64_at(&self.mmap, o + 8) as i64,
            hash: u64_at(&self.mmap, o + 16),
        }
    }

    /// Binary searches the path table.
    pub fn find_path(&self, path: &[u8]) -> Option<u32> {
        let (mut lo, mut hi) = (0u32, self.num_files);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match self.path(mid).cmp(path) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }

    #[inline]
    fn dict_entry(&self, i: u32) -> (u32, u32, u64) {
        let o = self.dict_off + DICT_LEN * i as usize;
        (
            u32_at(&self.mmap, o),
            u32_at(&self.mmap, o + 4),
            u64_at(&self.mmap, o + 8),
        )
    }

    fn postings_slice(&self, i: u32) -> &[u8] {
        let (_, _, off) = self.dict_entry(i);
        let start = self.postings_off + off as usize;
        let end = if i + 1 < self.num_trigrams {
            self.postings_off + self.dict_entry(i + 1).2 as usize
        } else {
            self.dict_off
        };
        &self.mmap[start..end]
    }

    fn find_trigram(&self, tri: u32) -> Option<u32> {
        let (mut lo, mut hi) = (0u32, self.num_trigrams);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let t = self.dict_entry(mid).0;
            if t < tri {
                lo = mid + 1;
            } else if t > tri {
                hi = mid;
            } else {
                return Some(mid);
            }
        }
        None
    }

    /// Number of files containing `tri` (0 if absent).
    pub fn doc_count(&self, tri: u32) -> u32 {
        self.find_trigram(tri)
            .map(|i| self.dict_entry(i).1)
            .unwrap_or(0)
    }

    /// Appends the file ids containing `tri` to `out`.
    pub fn postings(&self, tri: u32, out: &mut Vec<u32>) {
        if let Some(i) = self.find_trigram(tri) {
            let count = self.dict_entry(i).1 as usize;
            varint::decode_postings(self.postings_slice(i), count, out);
        }
    }

    /// Iterates the dictionary in trigram order as (trigram, doc count, index).
    pub fn dict_iter(&self) -> impl Iterator<Item = (u32, u32, u32)> + '_ {
        (0..self.num_trigrams).map(move |i| {
            let (t, c, _) = self.dict_entry(i);
            (t, c, i)
        })
    }

    /// Decodes the postings at dictionary index `i`.
    pub fn postings_at(&self, i: u32, out: &mut Vec<u32>) {
        let count = self.dict_entry(i).1 as usize;
        varint::decode_postings(self.postings_slice(i), count, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("seg-000001.idx");
        let mut w = SegmentWriter::create(&p).unwrap();
        w.add_file(
            b"a.rs",
            FileMeta {
                size: 1,
                mtime_ns: 2,
                hash: 3,
            },
        );
        w.add_file(
            b"b/c.rs",
            FileMeta {
                size: 4,
                mtime_ns: -5,
                hash: 6,
            },
        );
        w.add_postings(10, &[0, 1]).unwrap();
        w.add_postings(20, &[1]).unwrap();
        w.finish().unwrap();

        let s = Segment::open(&p).unwrap();
        assert_eq!(s.num_files(), 2);
        assert_eq!(s.path(1), b"b/c.rs");
        assert_eq!(s.meta(1).mtime_ns, -5);
        assert_eq!(s.find_path(b"a.rs"), Some(0));
        assert_eq!(s.find_path(b"zz"), None);
        let mut v = Vec::new();
        s.postings(10, &mut v);
        assert_eq!(v, vec![0, 1]);
        v.clear();
        s.postings(20, &mut v);
        assert_eq!(v, vec![1]);
        v.clear();
        s.postings(15, &mut v);
        assert!(v.is_empty());
        assert_eq!(s.doc_count(10), 2);
    }

    #[test]
    fn empty_segment() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("seg.idx");
        SegmentWriter::create(&p).unwrap().finish().unwrap();
        let s = Segment::open(&p).unwrap();
        assert_eq!(s.num_files(), 0);
        assert_eq!(s.find_path(b"x"), None);
    }
}
