//! Symbol sidecar files: `seg-NNNNNN.sym` next to `seg-NNNNNN.idx`.
//!
//! Layout (little endian, u32 arrays 4-byte aligned):
//!
//! ```text
//! header   64 bytes: magic "SCOUTSY1", version, nnames, nrecs,
//!                    offsets of blob, name_offs, recs, rec_starts, total len
//! blob     sorted unique names, concatenated
//! name_offs (nnames + 1) x u32 into blob
//! recs     nrecs x 12 bytes: file u32, line << 8 | kind u32, container u32
//! rec_starts (nnames + 1) x u32 into recs
//! ```
//!
//! Records of one name are contiguous, definitions before calls, then by
//! file and line. `container` is a name id (the enclosing function of a call)
//! or u32::MAX.

use std::collections::{BinaryHeap, HashMap};
use std::cmp::Reverse;
use std::fs::{self, File};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use memmap2::Mmap;
use scout_symbols::{Kind, Sym};

const MAGIC: &[u8; 8] = b"SCOUTSY1";
const VERSION: u32 = 1;
const HEADER: usize = 64;
const REC: usize = 12;
pub const NONE: u32 = u32::MAX;
const MAX_LINE: u32 = (1 << 24) - 1;

pub fn sym_path(idx: &Path) -> PathBuf {
    idx.with_extension("sym")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SymRec {
    pub file: u32,
    pub line: u32,
    pub kind: Kind,
    /// Name id of the enclosing function, for calls.
    pub container: Option<u32>,
}

fn sort_key(r: &[u32; 3]) -> (bool, u32, u32) {
    ((r[1] & 0xff) as u8 == Kind::Call as u8, r[0], r[1])
}

/// Streams a sym file: all names in sorted order, then records name by name.
struct Writer {
    tmp: PathBuf,
    out: PathBuf,
    w: BufWriter<File>,
    pos: u64,
    name_offs: Vec<u32>,
    blob_len: u32,
    rec_starts: Vec<u32>,
    nrecs: u32,
    off_name_offs: u64,
    off_recs: u64,
}

impl Writer {
    fn create(out: &Path) -> Result<Writer> {
        let tmp = out.with_extension("sym.tmp");
        let f = File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        let mut w = BufWriter::with_capacity(1 << 20, f);
        w.write_all(&[0u8; HEADER])?;
        Ok(Writer {
            tmp,
            out: out.to_path_buf(),
            w,
            pos: HEADER as u64,
            name_offs: vec![0],
            blob_len: 0,
            rec_starts: Vec::new(),
            nrecs: 0,
            off_name_offs: 0,
            off_recs: 0,
        })
    }

    fn add_name(&mut self, name: &[u8]) -> Result<()> {
        self.w.write_all(name)?;
        self.pos += name.len() as u64;
        self.blob_len += name.len() as u32;
        self.name_offs.push(self.blob_len);
        Ok(())
    }

    fn pad4(&mut self) -> Result<()> {
        while self.pos % 4 != 0 {
            self.w.write_all(&[0])?;
            self.pos += 1;
        }
        Ok(())
    }

    fn begin_records(&mut self) -> Result<()> {
        self.pad4()?;
        self.off_name_offs = self.pos;
        for &o in &self.name_offs {
            self.w.write_all(&o.to_le_bytes())?;
        }
        self.pos += 4 * self.name_offs.len() as u64;
        self.off_recs = self.pos;
        self.rec_starts.reserve(self.name_offs.len());
        Ok(())
    }

    /// Records for the next name id, already in sort order.
    fn add_records(&mut self, recs: &[[u32; 3]]) -> Result<()> {
        self.rec_starts.push(self.nrecs);
        for r in recs {
            for v in r {
                self.w.write_all(&v.to_le_bytes())?;
            }
        }
        self.nrecs += recs.len() as u32;
        self.pos += (recs.len() * REC) as u64;
        Ok(())
    }

    fn finish(mut self) -> Result<()> {
        let nnames = self.name_offs.len() as u32 - 1;
        if self.rec_starts.len() as u32 != nnames {
            bail!("sym writer: {} record groups for {} names", self.rec_starts.len(), nnames);
        }
        self.rec_starts.push(self.nrecs);
        let off_rec_starts = self.pos;
        for &o in &self.rec_starts {
            self.w.write_all(&o.to_le_bytes())?;
        }
        self.pos += 4 * self.rec_starts.len() as u64;
        let mut h = [0u8; HEADER];
        h[0..8].copy_from_slice(MAGIC);
        h[8..12].copy_from_slice(&VERSION.to_le_bytes());
        h[12..16].copy_from_slice(&nnames.to_le_bytes());
        h[16..20].copy_from_slice(&self.nrecs.to_le_bytes());
        h[24..32].copy_from_slice(&(HEADER as u64).to_le_bytes());
        h[32..40].copy_from_slice(&self.off_name_offs.to_le_bytes());
        h[40..48].copy_from_slice(&self.off_recs.to_le_bytes());
        h[48..56].copy_from_slice(&off_rec_starts.to_le_bytes());
        h[56..64].copy_from_slice(&self.pos.to_le_bytes());
        let mut f = self.w.into_inner().map_err(|e| e.into_error())?;
        f.seek(SeekFrom::Start(0))?;
        f.write_all(&h)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&self.tmp, &self.out)?;
        Ok(())
    }
}

/// Collects symbols in memory for one new segment.
#[derive(Default)]
pub struct SymBuilder {
    ids: HashMap<Box<[u8]>, u32>,
    names: Vec<Box<[u8]>>,
    /// name, file, line << 8 | kind, container
    recs: Vec<[u32; 4]>,
}

impl SymBuilder {
    pub fn new() -> SymBuilder {
        SymBuilder::default()
    }

    fn intern(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.ids.get(s.as_bytes()) {
            return id;
        }
        let id = self.names.len() as u32;
        let b: Box<[u8]> = s.as_bytes().into();
        self.names.push(b.clone());
        self.ids.insert(b, id);
        id
    }

    pub fn add(&mut self, file: u32, syms: &[Sym]) {
        for s in syms {
            let name = self.intern(&s.name);
            let container = s.container.as_deref().map(|c| self.intern(c)).unwrap_or(NONE);
            let line = s.line.min(MAX_LINE);
            self.recs.push([name, file, line << 8 | s.kind as u32, container]);
        }
    }

    pub fn len(&self) -> usize {
        self.recs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.recs.is_empty()
    }

    /// Rough heap footprint, for batch memory accounting.
    pub fn approx_bytes(&self) -> usize {
        self.recs.len() * 16 + self.names.len() * 48
    }

    pub fn clear(&mut self) {
        *self = SymBuilder::default();
    }

    pub fn write(&mut self, out: &Path) -> Result<()> {
        let mut order: Vec<u32> = (0..self.names.len() as u32).collect();
        order.sort_unstable_by(|&a, &b| self.names[a as usize].cmp(&self.names[b as usize]));
        let mut remap = vec![0u32; order.len()];
        for (new, &old) in order.iter().enumerate() {
            remap[old as usize] = new as u32;
        }
        for r in self.recs.iter_mut() {
            r[0] = remap[r[0] as usize];
            if r[3] != NONE {
                r[3] = remap[r[3] as usize];
            }
        }
        self.recs.sort_unstable_by_key(|r| (r[0], sort_key(&[r[1], r[2], r[3]])));
        let mut w = Writer::create(out)?;
        for &old in &order {
            w.add_name(&self.names[old as usize])?;
        }
        w.begin_records()?;
        let mut i = 0;
        let mut group: Vec<[u32; 3]> = Vec::new();
        for name in 0..order.len() as u32 {
            group.clear();
            while i < self.recs.len() && self.recs[i][0] == name {
                let r = self.recs[i];
                group.push([r[1], r[2], r[3]]);
                i += 1;
            }
            w.add_records(&group)?;
        }
        w.finish()
    }
}

pub struct SymFile {
    map: Mmap,
    nnames: u32,
    nrecs: u32,
    off_blob: usize,
    off_name_offs: usize,
    off_recs: usize,
    off_rec_starts: usize,
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}

impl SymFile {
    pub fn open(path: &Path) -> Result<SymFile> {
        let f = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        // SAFETY: sym files are immutable once renamed into place.
        let map = unsafe { Mmap::map(&f)? };
        if map.len() < HEADER || &map[0..8] != MAGIC {
            bail!("{} is not a scout symbol file", path.display());
        }
        if u32_at(&map, 8) != VERSION {
            bail!("{}: unsupported symbol file version", path.display());
        }
        let s = SymFile {
            nnames: u32_at(&map, 12),
            nrecs: u32_at(&map, 16),
            off_blob: u64_at(&map, 24) as usize,
            off_name_offs: u64_at(&map, 32) as usize,
            off_recs: u64_at(&map, 40) as usize,
            off_rec_starts: u64_at(&map, 48) as usize,
            map,
        };
        let total = u64_at(&s.map, 56) as usize;
        if total != s.map.len()
            || s.off_name_offs + 4 * (s.nnames as usize + 1) > s.map.len()
            || s.off_recs + REC * s.nrecs as usize > s.map.len()
            || s.off_rec_starts + 4 * (s.nnames as usize + 1) != s.map.len()
        {
            bail!("{} is truncated or corrupt", path.display());
        }
        Ok(s)
    }

    pub fn num_names(&self) -> u32 {
        self.nnames
    }

    pub fn num_records(&self) -> u32 {
        self.nrecs
    }

    pub fn byte_len(&self) -> usize {
        self.map.len()
    }

    pub fn name(&self, i: u32) -> &[u8] {
        let a = u32_at(&self.map, self.off_name_offs + 4 * i as usize) as usize;
        let b = u32_at(&self.map, self.off_name_offs + 4 * (i as usize + 1)) as usize;
        &self.map[self.off_blob + a..self.off_blob + b]
    }

    /// First name id whose name is >= `key`.
    pub fn lower_bound(&self, key: &[u8]) -> u32 {
        let (mut lo, mut hi) = (0u32, self.nnames);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.name(mid) < key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    pub fn find(&self, key: &[u8]) -> Option<u32> {
        let i = self.lower_bound(key);
        (i < self.nnames && self.name(i) == key).then_some(i)
    }

    fn raw(&self, r: u32) -> [u32; 3] {
        let o = self.off_recs + REC * r as usize;
        [u32_at(&self.map, o), u32_at(&self.map, o + 4), u32_at(&self.map, o + 8)]
    }

    fn rec_range(&self, name: u32) -> (u32, u32) {
        (
            u32_at(&self.map, self.off_rec_starts + 4 * name as usize),
            u32_at(&self.map, self.off_rec_starts + 4 * (name as usize + 1)),
        )
    }

    pub fn records(&self, name: u32) -> impl Iterator<Item = SymRec> + '_ {
        let (a, b) = self.rec_range(name);
        (a..b).filter_map(move |r| {
            let [file, lk, c] = self.raw(r);
            Some(SymRec {
                file,
                line: lk >> 8,
                kind: Kind::from_u8((lk & 0xff) as u8)?,
                container: (c != NONE).then_some(c),
            })
        })
    }
}

/// One input to a symbol merge: its file and the old -> new file id map
/// (u32::MAX for files that are dropped).
pub struct SymMergeInput<'a> {
    pub syms: &'a SymFile,
    pub remap: &'a [u32],
}

/// Merges symbol files, renumbering files and dropping records of files
/// that did not survive the segment merge.
pub fn merge_syms(inputs: &[SymMergeInput], out: &Path) -> Result<()> {
    let mut w = Writer::create(out)?;
    // Pass 1: union of names. name_map[i][old] = new.
    let mut name_map: Vec<Vec<u32>> = inputs.iter().map(|i| vec![NONE; i.syms.num_names() as usize]).collect();
    // For pass 2: which (input, old id) pairs carry each new id, in order.
    let mut sources: Vec<(u32, u32, u32)> = Vec::new(); // (new, input, old)
    let mut heap: BinaryHeap<Reverse<(&[u8], usize, u32)>> = BinaryHeap::new();
    for (ii, inp) in inputs.iter().enumerate() {
        if inp.syms.num_names() > 0 {
            heap.push(Reverse((inp.syms.name(0), ii, 0)));
        }
    }
    let mut next: u32 = 0;
    let mut last: Option<&[u8]> = None;
    while let Some(Reverse((name, ii, id))) = heap.pop() {
        if last != Some(name) {
            w.add_name(name)?;
            last = Some(name);
            next += 1;
        }
        let new = next - 1;
        name_map[ii][id as usize] = new;
        sources.push((new, ii as u32, id));
        let syms = inputs[ii].syms;
        if id + 1 < syms.num_names() {
            heap.push(Reverse((syms.name(id + 1), ii, id + 1)));
        }
    }
    w.begin_records()?;

    // Pass 2: records, name by name.
    let mut group: Vec<[u32; 3]> = Vec::new();
    let mut s = 0;
    for new in 0..next {
        group.clear();
        let mut nsrc = 0;
        while s < sources.len() && sources[s].0 == new {
            let (_, ii, old) = sources[s];
            let inp = &inputs[ii as usize];
            let (a, b) = inp.syms.rec_range(old);
            let before = group.len();
            for r in a..b {
                let [file, lk, c] = inp.syms.raw(r);
                let nf = inp.remap.get(file as usize).copied().unwrap_or(NONE);
                if nf == NONE {
                    continue;
                }
                let nc = if c == NONE { NONE } else { name_map[ii as usize][c as usize] };
                group.push([nf, lk, nc]);
            }
            if group.len() > before {
                nsrc += 1;
            }
            s += 1;
        }
        if nsrc > 1 {
            group.sort_unstable_by_key(sort_key);
        }
        w.add_records(&group)?;
    }
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(name: &str, kind: Kind, line: u32, container: Option<&str>) -> Sym {
        Sym { name: name.into(), kind, line, col: 0, container: container.map(Into::into) }
    }

    fn dump(f: &SymFile) -> Vec<(String, u32, u32, &'static str, Option<String>)> {
        let mut v = Vec::new();
        for n in 0..f.num_names() {
            for r in f.records(n) {
                v.push((
                    String::from_utf8_lossy(f.name(n)).into_owned(),
                    r.file,
                    r.line,
                    r.kind.name(),
                    r.container.map(|c| String::from_utf8_lossy(f.name(c)).into_owned()),
                ));
            }
        }
        v
    }

    #[test]
    fn build_read_merge() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.sym");
        let mut b = SymBuilder::new();
        b.add(0, &[sym("main", Kind::Function, 1, None), sym("helper", Kind::Call, 2, Some("main"))]);
        b.add(1, &[sym("helper", Kind::Function, 5, None)]);
        b.write(&a).unwrap();
        let fa = SymFile::open(&a).unwrap();
        assert_eq!(fa.num_names(), 2);
        assert_eq!(fa.find(b"helper"), Some(0));
        assert_eq!(fa.find(b"nope"), None);
        assert_eq!(
            dump(&fa),
            vec![
                ("helper".into(), 1, 5, "function", None),
                ("helper".into(), 0, 2, "call", Some("main".into())),
                ("main".into(), 0, 1, "function", None),
            ]
        );

        let c = dir.path().join("c.sym");
        let mut b = SymBuilder::new();
        b.add(0, &[sym("zed", Kind::Call, 9, Some("helper")), sym("helper", Kind::Function, 3, None)]);
        b.write(&c).unwrap();
        let fc = SymFile::open(&c).unwrap();

        // a's file 0 is dropped, file 1 becomes 0; c's file 0 becomes 1.
        let out = dir.path().join("m.sym");
        merge_syms(
            &[SymMergeInput { syms: &fa, remap: &[NONE, 0] }, SymMergeInput { syms: &fc, remap: &[1] }],
            &out,
        )
        .unwrap();
        let fm = SymFile::open(&out).unwrap();
        assert_eq!(
            dump(&fm),
            vec![
                ("helper".into(), 0, 5, "function", None),
                ("helper".into(), 1, 3, "function", None),
                ("zed".into(), 1, 9, "call", Some("helper".into())),
            ]
        );
        // "main" survives as a name with no records.
        assert_eq!(fm.find(b"main").map(|i| fm.records(i).count()), Some(0));
    }
}
