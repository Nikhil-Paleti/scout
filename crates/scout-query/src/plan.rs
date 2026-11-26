//! Regex to trigram query planning, after Russ Cox's codesearch
//! ("Regular Expression Matching with a Trigram Index", 2012).
//!
//! Each regex node is summarized by whether it can match the empty string,
//! its exact set of possible matches (if small), sets of possible prefixes
//! and suffixes, and a boolean trigram query every match must satisfy.
//! All strings are ASCII-lowercased because the index is.

use regex_syntax::hir::{Class, Hir, HirKind};

use scout_index::trigram::pack;

const MAX_EXACT: usize = 7;
const MAX_SET: usize = 20;
const MAX_CLASS: u32 = 64;
const MAX_EXACT_LEN: usize = 24;

/// A boolean query over trigrams.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Q {
    All,
    None,
    Tri(u32),
    And(Vec<Q>),
    Or(Vec<Q>),
}

impl Q {
    pub fn and(self, other: Q) -> Q {
        match (self, other) {
            (Q::None, _) | (_, Q::None) => Q::None,
            (Q::All, q) | (q, Q::All) => q,
            (a, b) => {
                let mut v = Vec::new();
                for q in [a, b] {
                    match q {
                        Q::And(xs) => v.extend(xs),
                        q => v.push(q),
                    }
                }
                v.sort();
                v.dedup();
                if v.len() == 1 {
                    v.pop().unwrap()
                } else {
                    Q::And(v)
                }
            }
        }
    }

    pub fn or(self, other: Q) -> Q {
        match (self, other) {
            (Q::All, _) | (_, Q::All) => Q::All,
            (Q::None, q) | (q, Q::None) => q,
            (a, b) => {
                let mut v = Vec::new();
                for q in [a, b] {
                    match q {
                        Q::Or(xs) => v.extend(xs),
                        q => v.push(q),
                    }
                }
                v.sort();
                v.dedup();
                // Absorption: (A and B) or A == A.
                let snapshot = v.clone();
                v.retain(|x| {
                    let ax = atoms(x);
                    !snapshot.iter().any(|y| y != x && atoms(y).iter().all(|a| ax.contains(a)))
                });
                if v.len() == 1 {
                    v.pop().unwrap()
                } else {
                    Q::Or(v)
                }
            }
        }
    }

    /// Human-readable form, for `--stats`.
    pub fn describe(&self) -> String {
        match self {
            Q::All => "ALL".into(),
            Q::None => "NONE".into(),
            Q::Tri(t) => {
                let b = scout_index::trigram::unpack(*t);
                format!("{:?}", String::from_utf8_lossy(&b))
            }
            Q::And(xs) => format!("({})", xs.iter().map(|q| q.describe()).collect::<Vec<_>>().join(" ")),
            Q::Or(xs) => format!("({})", xs.iter().map(|q| q.describe()).collect::<Vec<_>>().join("|")),
        }
    }
}

fn atoms(q: &Q) -> Vec<&Q> {
    match q {
        Q::And(xs) => xs.iter().collect(),
        q => vec![q],
    }
}

type Set = Vec<Vec<u8>>;

fn clean(s: &mut Set) {
    s.sort();
    s.dedup();
}

fn min_len(s: &Set) -> usize {
    s.iter().map(|x| x.len()).min().unwrap_or(0)
}

fn union(a: &Set, b: &Set) -> Set {
    let mut v: Set = a.iter().chain(b.iter()).cloned().collect();
    clean(&mut v);
    v
}

fn cross(a: &Set, b: &Set) -> Set {
    let mut v = Vec::with_capacity(a.len() * b.len());
    for x in a {
        for y in b {
            let mut s = x.clone();
            s.extend_from_slice(y);
            v.push(s);
        }
    }
    clean(&mut v);
    v
}

fn lower(b: &[u8]) -> Vec<u8> {
    b.iter().map(|c| c.to_ascii_lowercase()).collect()
}

/// q AND (OR over strings of (AND of the string's trigrams)).
fn and_trigrams(q: Q, set: &Set) -> Q {
    if set.is_empty() || min_len(set) < 3 {
        return q;
    }
    let mut or = Q::None;
    for s in set {
        let mut and = Q::All;
        for w in s.windows(3) {
            if w.contains(&b'\n') {
                continue;
            }
            and = and.and(Q::Tri(pack(w[0], w[1], w[2])));
        }
        or = or.or(and);
    }
    q.and(or)
}

#[derive(Clone, Debug)]
struct Info {
    can_empty: bool,
    exact: Option<Set>,
    prefix: Set,
    suffix: Set,
    q: Q,
}

impl Info {
    fn any_match() -> Info {
        Info { can_empty: true, exact: None, prefix: vec![vec![]], suffix: vec![vec![]], q: Q::All }
    }
    fn any_char() -> Info {
        Info { can_empty: false, exact: None, prefix: vec![vec![]], suffix: vec![vec![]], q: Q::All }
    }
    fn empty_string() -> Info {
        Info { can_empty: true, exact: Some(vec![vec![]]), prefix: vec![], suffix: vec![], q: Q::All }
    }
    fn no_match() -> Info {
        Info { can_empty: false, exact: Some(vec![]), prefix: vec![], suffix: vec![], q: Q::None }
    }
    fn exact(set: Set) -> Info {
        let can_empty = set.iter().any(|s| s.is_empty());
        Info { can_empty, exact: Some(set), prefix: vec![], suffix: vec![], q: Q::All }
    }

    fn add_exact(&mut self) {
        if let Some(ex) = &self.exact {
            let q = std::mem::replace(&mut self.q, Q::All);
            self.q = and_trigrams(q, ex);
        }
    }

    fn simplify(mut self, force: bool) -> Info {
        if let Some(mut ex) = self.exact.take() {
            clean(&mut ex);
            let ml = min_len(&ex);
            if ex.len() > MAX_EXACT || (ml >= 3 && force) || ml >= MAX_EXACT_LEN {
                self.exact = Some(ex.clone());
                self.add_exact();
                self.exact = None;
                for s in ex {
                    let n = s.len();
                    if n < 3 {
                        self.prefix.push(s.clone());
                        self.suffix.push(s);
                    } else {
                        self.prefix.push(s[..2].to_vec());
                        self.suffix.push(s[n - 2..].to_vec());
                    }
                }
            } else {
                self.exact = Some(ex);
            }
        }
        if self.exact.is_none() {
            let mut p = std::mem::take(&mut self.prefix);
            self.simplify_set(&mut p, false);
            self.prefix = p;
            let mut s = std::mem::take(&mut self.suffix);
            self.simplify_set(&mut s, true);
            self.suffix = s;
        }
        self
    }

    fn simplify_set(&mut self, t: &mut Set, is_suffix: bool) {
        clean(t);
        let q = std::mem::replace(&mut self.q, Q::All);
        self.q = and_trigrams(q, t);
        let mut n = 3usize;
        while n >= 1 && (n == 3 || t.len() > MAX_SET) {
            for s in t.iter_mut() {
                if s.len() >= n {
                    if is_suffix {
                        let k = s.len() - (n - 1);
                        *s = s[k..].to_vec();
                    } else {
                        s.truncate(n - 1);
                    }
                }
            }
            clean(t);
            n -= 1;
        }
        // Drop strings made redundant by a shorter prefix/suffix.
        let snapshot = t.clone();
        t.retain(|s| {
            !snapshot.iter().any(|o| {
                o.len() < s.len() && if is_suffix { s.ends_with(o) } else { s.starts_with(o) }
            })
        });
    }
}

fn concat(x: Info, y: Info) -> Info {
    let mut xy = Info {
        can_empty: x.can_empty && y.can_empty,
        exact: None,
        prefix: vec![],
        suffix: vec![],
        q: x.q.clone().and(y.q.clone()),
    };
    match (&x.exact, &y.exact) {
        (Some(xe), Some(ye)) => xy.exact = Some(cross(xe, ye)),
        _ => {
            xy.prefix = match &x.exact {
                Some(xe) => cross(xe, &y.prefix),
                None => {
                    if x.can_empty {
                        union(&x.prefix, &y.prefix)
                    } else {
                        x.prefix.clone()
                    }
                }
            };
            xy.suffix = match &y.exact {
                Some(ye) => cross(&x.suffix, ye),
                None => {
                    if y.can_empty {
                        union(&y.suffix, &x.suffix)
                    } else {
                        y.suffix.clone()
                    }
                }
            };
        }
    }
    if x.exact.is_none()
        && y.exact.is_none()
        && x.suffix.len() <= MAX_SET
        && y.prefix.len() <= MAX_SET
        && min_len(&x.suffix) + min_len(&y.prefix) >= 3
    {
        let q = std::mem::replace(&mut xy.q, Q::All);
        xy.q = and_trigrams(q, &cross(&x.suffix, &y.prefix));
    }
    xy.simplify(false)
}

fn alternate(mut x: Info, mut y: Info) -> Info {
    let mut xy = Info {
        can_empty: x.can_empty || y.can_empty,
        exact: None,
        prefix: vec![],
        suffix: vec![],
        q: Q::All,
    };
    match (x.exact.clone(), y.exact.clone()) {
        (Some(xe), Some(ye)) => xy.exact = Some(union(&xe, &ye)),
        (Some(xe), None) => {
            xy.prefix = union(&xe, &y.prefix);
            xy.suffix = union(&xe, &y.suffix);
            x.add_exact();
        }
        (None, Some(ye)) => {
            xy.prefix = union(&x.prefix, &ye);
            xy.suffix = union(&x.suffix, &ye);
            y.add_exact();
        }
        (None, None) => {
            xy.prefix = union(&x.prefix, &y.prefix);
            xy.suffix = union(&x.suffix, &y.suffix);
        }
    }
    xy.q = x.q.or(y.q);
    xy.simplify(false)
}

fn class_info(c: &Class) -> Info {
    let mut set: Set = Vec::new();
    match c {
        Class::Unicode(u) => {
            let total: u32 = u.ranges().iter().map(|r| r.end() as u32 - r.start() as u32 + 1).sum();
            if total > MAX_CLASS {
                return Info::any_char();
            }
            for r in u.ranges() {
                for cp in (r.start() as u32)..=(r.end() as u32) {
                    if let Some(ch) = char::from_u32(cp) {
                        let mut buf = [0u8; 4];
                        set.push(lower(ch.encode_utf8(&mut buf).as_bytes()));
                    }
                }
            }
        }
        Class::Bytes(b) => {
            let total: u32 = b.ranges().iter().map(|r| r.end() as u32 - r.start() as u32 + 1).sum();
            if total > MAX_CLASS {
                return Info::any_char();
            }
            for r in b.ranges() {
                for v in r.start()..=r.end() {
                    set.push(vec![v.to_ascii_lowercase()]);
                }
            }
        }
    }
    if set.is_empty() {
        return Info::no_match();
    }
    clean(&mut set);
    Info::exact(set).simplify(false)
}

fn analyze(h: &Hir) -> Info {
    match h.kind() {
        HirKind::Empty | HirKind::Look(_) => Info::empty_string(),
        HirKind::Literal(lit) => Info::exact(vec![lower(&lit.0)]).simplify(false),
        HirKind::Class(c) => class_info(c),
        HirKind::Capture(c) => analyze(&c.sub),
        HirKind::Repetition(r) => {
            if r.min == 0 && r.max == Some(1) {
                return alternate(analyze(&r.sub), Info::empty_string());
            }
            if r.min == 0 {
                return Info::any_match();
            }
            let x = analyze(&r.sub);
            if r.max == Some(r.min) && r.min <= 4 && x.exact.is_some() {
                let mut info = x.clone();
                for _ in 1..r.min {
                    info = concat(info, x.clone());
                }
                return info;
            }
            let mut info = x;
            if let Some(ex) = info.exact.take() {
                info.prefix = ex.clone();
                info.suffix = ex;
            }
            info.simplify(false)
        }
        HirKind::Concat(subs) => {
            let mut it = subs.iter();
            let mut info = match it.next() {
                Some(s) => analyze(s),
                None => return Info::empty_string(),
            };
            for s in it {
                info = concat(info, analyze(s));
            }
            info
        }
        HirKind::Alternation(subs) => {
            let mut it = subs.iter();
            let mut info = match it.next() {
                Some(s) => analyze(s),
                None => return Info::no_match(),
            };
            for s in it {
                info = alternate(info, analyze(s));
            }
            info
        }
    }
}

/// Plans the trigram query every match of `hir` must satisfy.
pub fn plan(hir: &Hir) -> Q {
    let mut info = analyze(hir).simplify(true);
    info.add_exact();
    info.q
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(re: &str) -> String {
        let hir = regex_syntax::ParserBuilder::new().build().parse(re).unwrap();
        plan(&hir).describe()
    }

    fn pi(re: &str) -> String {
        let hir = regex_syntax::ParserBuilder::new().case_insensitive(true).build().parse(re).unwrap();
        plan(&hir).describe()
    }

    #[test]
    fn literals() {
        assert_eq!(p("abc"), "\"abc\"");
        assert_eq!(p("abcd"), "(\"abc\" \"bcd\")");
        assert_eq!(p("ab"), "ALL");
        // Case folding produces the same plan.
        assert_eq!(p("ABCD"), p("abcd"));
        assert_eq!(pi("abcd"), p("abcd"));
    }

    #[test]
    fn alternation_and_classes() {
        assert_eq!(p("abc|xyz"), "(\"abc\"|\"xyz\")");
        assert_eq!(p("ab[cd]e"), "((\"abc\" \"bce\")|(\"abd\" \"bde\"))");
        assert_eq!(p("a.b"), "ALL");
        assert_eq!(p(r"\w+"), "ALL");
    }

    #[test]
    fn concat_across_wildcards() {
        assert_eq!(p("hello.*world"), "(\"ell\" \"hel\" \"llo\" \"orl\" \"rld\" \"wor\")");
        assert_eq!(p(r"func\s+main"), "(\"ain\" \"fun\" \"mai\" \"unc\")");
    }

    #[test]
    fn optional_and_repeat() {
        assert_eq!(p("colou?r"), "((\"col\" \"lor\" \"olo\")|(\"col\" \"lou\" \"olo\" \"our\"))");
        // x+ keeps x's trigrams.
        assert_eq!(p("(abc)+"), "\"abc\"");
        assert_eq!(p("(abc)*"), "ALL");
    }
}
