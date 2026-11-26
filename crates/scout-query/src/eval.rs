//! Evaluates a trigram query against one segment's posting lists.

use scout_index::Segment;

use crate::plan::Q;

/// Candidate file ids, or every file.
#[derive(Debug, PartialEq, Eq)]
pub enum Cand {
    All,
    Ids(Vec<u32>),
}

fn cost(seg: &Segment, q: &Q) -> u64 {
    match q {
        Q::All => u64::MAX,
        Q::None => 0,
        Q::Tri(t) => seg.doc_count(*t) as u64,
        Q::And(xs) => xs.iter().map(|x| cost(seg, x)).min().unwrap_or(u64::MAX),
        Q::Or(xs) => xs.iter().map(|x| cost(seg, x)).fold(0u64, |a, b| a.saturating_add(b)),
    }
}

/// Intersects two sorted id lists, galloping when sizes are lopsided.
pub fn intersect(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (small, big) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let mut out = Vec::with_capacity(small.len());
    if small.is_empty() {
        return out;
    }
    if big.len() / small.len() > 16 {
        let mut lo = 0;
        for &x in small {
            match big[lo..].binary_search(&x) {
                Ok(i) => {
                    out.push(x);
                    lo += i + 1;
                }
                Err(i) => lo += i,
            }
            if lo >= big.len() {
                break;
            }
        }
    } else {
        let (mut i, mut j) = (0, 0);
        while i < small.len() && j < big.len() {
            match small[i].cmp(&big[j]) {
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
                std::cmp::Ordering::Equal => {
                    out.push(small[i]);
                    i += 1;
                    j += 1;
                }
            }
        }
    }
    out
}

fn union_into(acc: Vec<u32>, b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(acc.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < acc.len() && j < b.len() {
        match acc[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => {
                out.push(acc[i]);
                i += 1
            }
            std::cmp::Ordering::Greater => {
                out.push(b[j]);
                j += 1
            }
            std::cmp::Ordering::Equal => {
                out.push(acc[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out.extend_from_slice(&acc[i..]);
    out.extend_from_slice(&b[j..]);
    out
}

pub fn eval(seg: &Segment, q: &Q) -> Cand {
    match q {
        Q::All => Cand::All,
        Q::None => Cand::Ids(Vec::new()),
        Q::Tri(t) => {
            let mut v = Vec::new();
            seg.postings(*t, &mut v);
            Cand::Ids(v)
        }
        Q::And(xs) => {
            let mut order: Vec<(u64, &Q)> = xs.iter().map(|x| (cost(seg, x), x)).collect();
            order.sort_by_key(|(c, _)| *c);
            let mut acc: Option<Vec<u32>> = None;
            for (_, x) in order {
                match eval(seg, x) {
                    Cand::All => {}
                    Cand::Ids(ids) => {
                        let next = match acc {
                            None => ids,
                            Some(a) => intersect(&a, &ids),
                        };
                        if next.is_empty() {
                            return Cand::Ids(next);
                        }
                        acc = Some(next);
                    }
                }
            }
            match acc {
                Some(a) => Cand::Ids(a),
                None => Cand::All,
            }
        }
        Q::Or(xs) => {
            let mut acc: Vec<u32> = Vec::new();
            for x in xs {
                match eval(seg, x) {
                    Cand::All => return Cand::All,
                    Cand::Ids(ids) => acc = union_into(acc, &ids),
                }
            }
            Cand::Ids(acc)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_ops() {
        assert_eq!(intersect(&[1, 3, 5, 7], &[3, 4, 5]), vec![3, 5]);
        let big: Vec<u32> = (0..1000).collect();
        assert_eq!(intersect(&[5, 500, 2000], &big), vec![5, 500]);
        assert_eq!(union_into(vec![1, 4], &[2, 4, 9]), vec![1, 2, 4, 9]);
    }
}
