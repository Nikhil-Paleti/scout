//! Merges segments into one, dropping tombstoned files and keeping only the
//! newest copy of each path. Used to finish a batched build and to compact
//! delta segments.

use std::path::Path;

use anyhow::Result;
use roaring::RoaringBitmap;

use crate::segment::{Segment, SegmentWriter};
use crate::symfile::{merge_syms, sym_path, SymFile, SymMergeInput};

pub struct MergeInput<'a> {
    pub seg: &'a Segment,
    pub del: Option<&'a RoaringBitmap>,
    pub syms: Option<&'a SymFile>,
}

/// A merge writes a symbol sidecar only when every input has one.
pub fn merge_has_syms(inputs: &[MergeInput]) -> bool {
    !inputs.is_empty() && inputs.iter().all(|i| i.syms.is_some())
}

/// Merges `inputs` (oldest first) into a new segment at `out`. Returns the
/// number of files written.
/// Symbols are merged into `out`'s `.sym` sidecar when every input has them.
pub fn merge(inputs: &[MergeInput], out: &Path) -> Result<u32> {
    // Live entries, newest copy of each path wins.
    let mut live: Vec<(&[u8], u32, u32)> = Vec::new();
    for (ii, inp) in inputs.iter().enumerate() {
        for id in 0..inp.seg.num_files() {
            if inp.del.map(|d| d.contains(id)).unwrap_or(false) {
                continue;
            }
            live.push((inp.seg.path(id), ii as u32, id));
        }
    }
    let presorted = live.windows(2).all(|w| w[0].0 < w[1].0);
    if !presorted {
        live.sort_unstable_by(|a, b| a.0.cmp(b.0).then(b.1.cmp(&a.1)));
        live.dedup_by(|later, earlier| later.0 == earlier.0);
    }

    let mut remap: Vec<Vec<u32>> = inputs
        .iter()
        .map(|i| vec![u32::MAX; i.seg.num_files() as usize])
        .collect();
    let mut w = SegmentWriter::create(out)?;
    for (new_id, &(path, ii, id)) in live.iter().enumerate() {
        remap[ii as usize][id as usize] = new_id as u32;
        w.add_file(path, inputs[ii as usize].seg.meta(id));
    }
    let nfiles = live.len() as u32;
    drop(live);

    let mut iters: Vec<_> = inputs
        .iter()
        .map(|i| i.seg.dict_iter().peekable())
        .collect();
    let mut acc: Vec<u32> = Vec::new();
    let mut tmp: Vec<u32> = Vec::new();
    loop {
        let mut min: Option<u32> = None;
        for it in iters.iter_mut() {
            if let Some(&(t, _, _)) = it.peek() {
                min = Some(min.map_or(t, |m: u32| m.min(t)));
            }
        }
        let Some(tri) = min else { break };
        acc.clear();
        let mut sources = 0;
        for (ii, it) in iters.iter_mut().enumerate() {
            if let Some(&(t, _, di)) = it.peek() {
                if t == tri {
                    it.next();
                    tmp.clear();
                    inputs[ii].seg.postings_at(di, &mut tmp);
                    let rm = &remap[ii];
                    let before = acc.len();
                    acc.extend(
                        tmp.iter()
                            .map(|&id| rm[id as usize])
                            .filter(|&n| n != u32::MAX),
                    );
                    if acc.len() > before {
                        sources += 1;
                    }
                }
            }
        }
        if sources > 1 {
            acc.sort_unstable();
        }
        w.add_postings(tri, &acc)?;
    }
    w.finish()?;
    if merge_has_syms(inputs) {
        let sym_inputs: Vec<SymMergeInput> = inputs
            .iter()
            .zip(&remap)
            .map(|(i, r)| SymMergeInput {
                syms: i.syms.unwrap(),
                remap: r,
            })
            .collect();
        merge_syms(&sym_inputs, &sym_path(out))?;
    }
    Ok(nfiles)
}
