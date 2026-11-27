//! Query execution: parse, plan, collect candidates per segment, verify
//! against the working tree.
//!
//! Queries follow Google Code Search (see `filters`): terms are ANDed per
//! file, a plain term matches the path or the contents, and files whose
//! path matches come back first as `kind: "file"` hits, then line hits.

pub mod eval;
pub mod files;
pub mod filters;
pub mod plan;
pub mod regions;
mod symbols;

use std::collections::{BTreeMap, HashSet};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, MmapChoice, Searcher, SearcherBuilder, Sink, SinkMatch};
use rayon::prelude::*;

use scout_index::{Index, Segment};

use eval::Cand;
pub use filters::{Case, Expr, TermKind};
use plan::Q;
use regions::Regions;

#[derive(Clone, Debug)]
pub struct SearchOptions {
    /// Default case for terms without `case:`. Google's default is
    /// insensitive.
    pub case: Case,
    pub fixed: bool,
    pub word: bool,
    pub max_matches: usize,
    pub threads: usize,
    /// Path (relative to the root) of the file the user is in, for ranking.
    pub near: Option<String>,
    /// Parse the query syntax (filters, AND/OR, quotes). Off: the whole
    /// query is one regex.
    pub filters: bool,
    /// Plain terms also match file paths, and matching files are returned
    /// first as file hits. Off: terms match contents only (ripgrep style).
    pub paths: bool,
    /// Cap on file hits when the query also matches lines.
    pub max_file_hits: usize,
    /// Cap on bytes of line text returned per hit.
    pub max_line_bytes: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        SearchOptions {
            case: Case::Insensitive,
            fixed: false,
            word: false,
            max_matches: 5000,
            threads: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4),
            near: None,
            filters: true,
            paths: true,
            max_file_hits: 50,
            max_line_bytes: 2000,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Hit {
    pub path: String,
    /// 1-based; 1 for file hits.
    pub line: u64,
    /// 1-based byte column of the first match.
    pub col: u64,
    /// Empty for file hits.
    pub text: Vec<u8>,
    /// Byte ranges of every match within `text` (within `path` for file hits).
    pub ranges: Vec<(usize, usize)>,
    /// `file` for a file-name hit; for symbol hits the kind (function,
    /// call, ...). `None` for a text line.
    pub kind: Option<&'static str>,
    /// For symbol hits: the symbol's name and, for calls, the function the
    /// call is in.
    pub symbol: Option<String>,
    pub container: Option<String>,
}

impl Hit {
    pub fn is_file(&self) -> bool {
        self.kind == Some("file")
    }
}

#[derive(Clone, Debug, Default)]
pub struct SearchStats {
    pub plan: String,
    pub segments: usize,
    pub live_files: u64,
    pub candidates: usize,
    pub files_searched: usize,
    pub files_matched: usize,
    pub matches: usize,
    pub file_hits: usize,
    pub truncated: bool,
    pub plan_time: Duration,
    pub candidate_time: Duration,
    pub total_time: Duration,
    pub first_hit: Option<Duration>,
    /// Symbol search ran while some segments still lack symbols.
    pub symbols_incomplete: bool,
}

pub struct CTerm {
    pub kind: TermKind,
    /// Content matcher (line oriented, or across lines with `pcre:yes`).
    pub matcher: Option<RegexMatcher>,
    /// Path matcher for plain and `file:`/`lang:` terms.
    pub path: Option<RegexMatcher>,
    pub plan: Q,
}

/// A compiled query: matchers per term, the boolean expression and the
/// trigram plans.
pub struct Compiled {
    pub terms: Vec<CTerm>,
    pub expr: Expr,
    pub pcre: bool,
    /// Terms whose matches are shown (not under `-`) and read contents.
    positive_content: Vec<usize>,
    /// Terms not under `-` that can match a path.
    positive_path: Vec<usize>,
    /// A `-` covers a term that reads contents.
    neg_content: bool,
    needs_regions: bool,
}

pub(crate) fn has_upper(s: &str) -> bool {
    // Mirrors ripgrep's smart case closely enough: an uppercase literal
    // character anywhere (escapes like \W or \S also count; that only makes
    // the search case sensitive, never wrong).
    s.chars().any(|c| c.is_uppercase())
}

fn insensitive(case: Case, pattern: &str) -> bool {
    match case {
        Case::Sensitive => false,
        Case::Insensitive => true,
        Case::Smart => !has_upper(pattern),
    }
}

pub fn parse(query: &str, opts: &SearchOptions) -> Result<filters::Parsed> {
    Ok(if opts.filters {
        filters::parse(query)?
    } else {
        filters::Parsed::raw(query)
    })
}

pub fn compile(query: &str, opts: &SearchOptions) -> Result<Compiled> {
    compile_parsed(&parse(query, opts)?, opts)
}

pub fn compile_parsed(parsed: &filters::Parsed, opts: &SearchOptions) -> Result<Compiled> {
    let mut terms = Vec::with_capacity(parsed.terms.len());
    for t in &parsed.terms {
        let kind = if t.kind == TermKind::Text && !opts.paths {
            TermKind::Content
        } else {
            t.kind
        };
        let mut pattern = if opts.fixed && !t.literal && kind != TermKind::Path {
            regex_syntax::escape(&t.pattern)
        } else {
            t.pattern.clone()
        };
        if opts.word && kind != TermKind::Path {
            pattern = format!(r"\b(?:{})\b", pattern);
        }
        let ins = insensitive(t.case.unwrap_or(opts.case), &pattern);
        let matcher = if kind.reads_content() {
            let mut b = RegexMatcherBuilder::new();
            b.case_insensitive(ins).multi_line(true);
            if !parsed.pcre {
                b.line_terminator(Some(b'\n'));
            }
            Some(
                b.build(&pattern)
                    .with_context(|| format!("invalid regex `{}`", pattern))?,
            )
        } else {
            None
        };
        let path = if matches!(kind, TermKind::Text | TermKind::Path) {
            Some(
                RegexMatcherBuilder::new()
                    .case_insensitive(ins)
                    .build(&pattern)
                    .with_context(|| format!("invalid regex `{}`", pattern))?,
            )
        } else {
            None
        };
        let plan = if kind.reads_content() {
            let hir = regex_syntax::ParserBuilder::new()
                .case_insensitive(ins)
                .multi_line(true)
                .utf8(false)
                .build()
                .parse(&pattern)
                .with_context(|| format!("invalid regex `{}`", pattern))?;
            plan::plan(&hir)
        } else {
            Q::All
        };
        terms.push(CTerm {
            kind,
            matcher,
            path,
            plan,
        });
    }
    let positive = parsed.positive_terms();
    let positive_content: Vec<usize> = positive
        .iter()
        .copied()
        .filter(|&i| terms[i].kind.reads_content())
        .collect();
    let positive_path: Vec<usize> = positive
        .iter()
        .copied()
        .filter(|&i| terms[i].path.is_some())
        .collect();
    let mut all_content = Vec::new();
    for (i, t) in terms.iter().enumerate() {
        if t.kind.reads_content() {
            all_content.push(i);
        }
    }
    let neg_content = all_content.iter().any(|i| !positive_content.contains(i));
    let needs_regions = terms
        .iter()
        .any(|t| matches!(t.kind, TermKind::Comment | TermKind::Usage));
    let c = Compiled {
        terms,
        expr: parsed.expr.clone(),
        pcre: parsed.pcre,
        positive_content,
        positive_path,
        neg_content,
        needs_regions,
    };
    if parsed.sym.is_some() {
        for e in c.conjuncts() {
            let (p, t) = c.term_kinds(e);
            if p && t {
                bail!("with sym:/calls:, keep file:/lang: filters and search words in separate groups");
            }
        }
    }
    Ok(c)
}

impl Compiled {
    fn conjuncts(&self) -> Vec<&Expr> {
        match &self.expr {
            Expr::And(xs) => xs.iter().collect(),
            Expr::True => Vec::new(),
            e => vec![e],
        }
    }

    /// (has path-only terms, has content terms) under `e`.
    fn term_kinds(&self, e: &Expr) -> (bool, bool) {
        match e {
            Expr::True => (false, false),
            Expr::Term(i) => {
                let k = self.terms[*i].kind;
                (k == TermKind::Path, k != TermKind::Path)
            }
            Expr::Not(x) => self.term_kinds(x),
            Expr::And(xs) | Expr::Or(xs) => xs.iter().fold((false, false), |a, x| {
                let b = self.term_kinds(x);
                (a.0 || b.0, a.1 || b.1)
            }),
        }
    }

    fn eval(&self, e: &Expr, val: &dyn Fn(usize) -> bool) -> bool {
        match e {
            Expr::True => true,
            Expr::Term(i) => val(*i),
            Expr::Not(x) => !self.eval(x, val),
            Expr::And(xs) => xs.iter().all(|x| self.eval(x, val)),
            Expr::Or(xs) => xs.iter().any(|x| self.eval(x, val)),
        }
    }

    /// Three-valued evaluation from the path alone: `None` when it depends
    /// on the contents.
    fn eval3(&self, e: &Expr, pm: &[bool]) -> Option<bool> {
        match e {
            Expr::True => Some(true),
            Expr::Term(i) => match self.terms[*i].kind {
                TermKind::Path => Some(pm[*i]),
                TermKind::Text if pm[*i] => Some(true),
                _ => None,
            },
            Expr::Not(x) => self.eval3(x, pm).map(|b| !b),
            Expr::And(xs) => {
                let mut unknown = false;
                for x in xs {
                    match self.eval3(x, pm) {
                        Some(false) => return Some(false),
                        None => unknown = true,
                        _ => {}
                    }
                }
                if unknown {
                    None
                } else {
                    Some(true)
                }
            }
            Expr::Or(xs) => {
                let mut unknown = false;
                for x in xs {
                    match self.eval3(x, pm) {
                        Some(true) => return Some(true),
                        None => unknown = true,
                        _ => {}
                    }
                }
                if unknown {
                    None
                } else {
                    Some(false)
                }
            }
        }
    }

    /// Whether the path alone satisfies the query: content terms count as
    /// unmatched, except under `-`, where `full` decides.
    fn path_eval(&self, e: &Expr, pm: &[bool], full: &dyn Fn(&Expr) -> bool) -> bool {
        match e {
            Expr::True => true,
            Expr::Term(i) => match self.terms[*i].kind {
                TermKind::Path | TermKind::Text => pm[*i],
                _ => false,
            },
            Expr::Not(x) => !full(x),
            Expr::And(xs) => xs.iter().all(|x| self.path_eval(x, pm, full)),
            Expr::Or(xs) => xs.iter().any(|x| self.path_eval(x, pm, full)),
        }
    }

    fn path_matches(&self, path: &[u8]) -> Vec<bool> {
        self.terms
            .iter()
            .map(|t| {
                t.path
                    .as_ref()
                    .is_some_and(|m| m.is_match(path).unwrap_or(false))
            })
            .collect()
    }

    /// Path check for the candidate step: false only when the path rules
    /// the file out.
    pub fn path_ok(&self, path: &[u8]) -> bool {
        self.eval3(&self.expr, &self.path_matches(path)) != Some(false)
    }

    /// For symbol queries: the conjuncts made of `file:`/`lang:` terms.
    pub(crate) fn sym_path_ok(&self, path: &[u8]) -> bool {
        let pm = self.path_matches(path);
        self.conjuncts()
            .into_iter()
            .filter(|e| !self.term_kinds(e).1)
            .all(|e| self.eval(e, &|i| pm[i]))
    }

    pub(crate) fn has_line_filter(&self) -> bool {
        self.conjuncts().into_iter().any(|e| self.term_kinds(e).1)
    }

    /// For symbol queries: the conjuncts with search words, against a line.
    pub(crate) fn sym_line_ok(&self, line: &[u8]) -> bool {
        self.conjuncts()
            .into_iter()
            .filter(|e| self.term_kinds(e).1)
            .all(|e| {
                self.eval(e, &|i| {
                    self.terms[i]
                        .matcher
                        .as_ref()
                        .is_some_and(|m| m.is_match(line).unwrap_or(false))
                })
            })
    }

    pub fn describe(&self) -> String {
        fn go(c: &Compiled, e: &Expr) -> String {
            match e {
                Expr::True => "all".into(),
                Expr::Term(i) => {
                    let t = &c.terms[*i];
                    match t.kind {
                        TermKind::Path => "path".into(),
                        TermKind::Text => format!("(path|{})", t.plan.describe()),
                        _ => t.plan.describe(),
                    }
                }
                Expr::Not(_) => "all".into(),
                Expr::And(xs) => format!(
                    "({})",
                    xs.iter()
                        .map(|x| go(c, x))
                        .collect::<Vec<_>>()
                        .join(" AND ")
                ),
                Expr::Or(xs) => format!(
                    "({})",
                    xs.iter().map(|x| go(c, x)).collect::<Vec<_>>().join(" OR ")
                ),
            }
        }
        go(self, &self.expr)
    }

    /// Candidate ids in one segment. Plain terms take the files whose
    /// contents may match (trigrams) plus the files whose path matches.
    fn seg_cands(&self, seg: &Segment, e: &Expr) -> Cand {
        let path_ids = |i: usize| -> Vec<u32> {
            let m = self.terms[i].path.as_ref().unwrap();
            (0..seg.num_files())
                .filter(|&id| m.is_match(seg.path(id)).unwrap_or(false))
                .collect()
        };
        match e {
            Expr::True | Expr::Not(_) => Cand::All,
            Expr::Term(i) => {
                let t = &self.terms[*i];
                match t.kind {
                    TermKind::Path => Cand::Ids(path_ids(*i)),
                    TermKind::Text => match eval::eval(seg, &t.plan) {
                        Cand::All => Cand::All,
                        Cand::Ids(ids) => Cand::Ids(eval::union_into(ids, &path_ids(*i))),
                    },
                    _ => eval::eval(seg, &t.plan),
                }
            }
            Expr::And(xs) => {
                let mut acc: Option<Vec<u32>> = None;
                // Cheap trigram terms first so an empty result stops early.
                let mut order: Vec<&Expr> = xs.iter().collect();
                order.sort_by_key(|x| !matches!(x, Expr::Term(i) if self.terms[*i].kind.reads_content() && self.terms[*i].kind != TermKind::Text));
                for x in order {
                    if let Cand::Ids(ids) = self.seg_cands(seg, x) {
                        let next = match acc {
                            None => ids,
                            Some(a) => eval::intersect(&a, &ids),
                        };
                        if next.is_empty() {
                            return Cand::Ids(next);
                        }
                        acc = Some(next);
                    }
                }
                acc.map(Cand::Ids).unwrap_or(Cand::All)
            }
            Expr::Or(xs) => {
                let mut acc = Vec::new();
                for x in xs {
                    match self.seg_cands(seg, x) {
                        Cand::All => return Cand::All,
                        Cand::Ids(ids) => acc = eval::union_into(acc, &ids),
                    }
                }
                Cand::Ids(acc)
            }
        }
    }
}

fn is_lowrank(path: &str) -> u32 {
    let mut p = 0;
    for part in path.split('/') {
        match part {
            "vendor" | "third_party" | "thirdparty" | "external" | "node_modules" | "deps" => {
                p += 4
            }
            "test" | "tests" | "testdata" | "testing" | "__tests__" | "spec" => p += 2,
            _ => {}
        }
    }
    let file = path.rsplit('/').next().unwrap_or(path);
    if file.contains("_test.")
        || file.contains(".test.")
        || file.contains("_spec.")
        || file.starts_with("test_")
    {
        p += 2;
    }
    p
}

fn shared_dirs(a: &str, near: &str) -> u32 {
    let ad = a.rsplit_once('/').map(|x| x.0).unwrap_or("");
    let nd = near.rsplit_once('/').map(|x| x.0).unwrap_or("");
    ad.split('/')
        .zip(nd.split('/'))
        .take_while(|(x, y)| x == y && !x.is_empty())
        .count() as u32
}

/// Lower is better: near the current file first, vendored and test code last.
pub(crate) fn path_rank(p: &str, near: Option<&str>) -> u32 {
    let near_bonus = near.map(|n| shared_dirs(p, n)).unwrap_or(0);
    1000 + is_lowrank(p) * 8 - near_bonus.min(100) * 4
}

/// Ranks candidate files by `path_rank`, path order otherwise.
fn rank(paths: &mut [(u32, String)], near: Option<&str>) {
    for (score, p) in paths.iter_mut() {
        *score = path_rank(p, near);
    }
    paths.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
}

struct HitSink<'a> {
    matcher: &'a RegexMatcher,
    hits: Vec<Hit>,
    path: &'a str,
    max_line: usize,
    limit: usize,
    kind: TermKind,
    regions: Option<&'a Regions>,
}

impl Sink for HitSink<'_> {
    type Error = io::Error;

    fn matched(&mut self, _s: &Searcher, m: &SinkMatch<'_>) -> Result<bool, io::Error> {
        let bytes = m.bytes();
        let off = m.absolute_byte_offset() as usize;
        let mut line = &bytes[..bytes
            .iter()
            .position(|&b| b == b'\n')
            .unwrap_or(bytes.len())];
        if line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        let mut ranges = Vec::new();
        let _ = self.matcher.find_iter(bytes, |mt| {
            let (s, e) = (mt.start(), mt.end());
            if s > line.len() {
                return false;
            }
            let keep = match (self.kind, self.regions) {
                (TermKind::Comment, Some(r)) => r.in_comment(off + s, off + e),
                (TermKind::Comment, None) => false,
                (TermKind::Usage, Some(r)) => r.in_code(off + s, off + e),
                _ => true,
            };
            if keep {
                ranges.push((s, e.min(line.len())));
            }
            ranges.len() < 64
        });
        if ranges.is_empty() && matches!(self.kind, TermKind::Comment | TermKind::Usage) {
            return Ok(true);
        }
        let col = ranges.first().map(|r| r.0 as u64 + 1).unwrap_or(1);
        let text = if line.len() > self.max_line {
            line[..self.max_line].to_vec()
        } else {
            line.to_vec()
        };
        ranges.retain(|r| r.1 <= text.len());
        self.hits.push(Hit {
            path: self.path.to_string(),
            line: m.line_number().unwrap_or(0),
            col,
            text,
            ranges,
            kind: None,
            symbol: None,
            container: None,
        });
        Ok(self.hits.len() < self.limit)
    }
}

fn searcher(multi_line: bool) -> Searcher {
    SearcherBuilder::new()
        .line_number(true)
        .multi_line(multi_line)
        .binary_detection(BinaryDetection::quit(0))
        // Candidates are mostly small source files, where read() beats
        // setting up a mapping per file (ripgrep makes the same call).
        .memory_map(MmapChoice::never())
        .build()
}

/// Runs `query` against `index`, calling `emit` for each hit in rank order
/// until it returns false or the match cap is reached: file hits first,
/// then line hits.
pub fn search(
    index: &Index,
    root: &Path,
    query: &str,
    opts: &SearchOptions,
    emit: &mut dyn FnMut(&Hit) -> bool,
) -> Result<SearchStats> {
    let t0 = Instant::now();
    let parsed = parse(query, opts)?;
    let compiled = compile_parsed(&parsed, opts)?;
    if parsed.sym.is_some() {
        return symbols::search(index, root, &parsed, &compiled, opts, emit);
    }
    let mut stats = SearchStats {
        plan: compiled.describe(),
        segments: index.segments.len(),
        live_files: index.live_count(),
        plan_time: t0.elapsed(),
        ..Default::default()
    };

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(opts.threads.max(1))
        .build()?;
    let per_seg: Vec<Vec<(u32, String)>> = pool.install(|| {
        index
            .segments
            .par_iter()
            .map(|s| {
                let ids: Box<dyn Iterator<Item = u32>> =
                    match compiled.seg_cands(&s.seg, &compiled.expr) {
                        Cand::All => Box::new(0..s.seg.num_files()),
                        Cand::Ids(v) => Box::new(v.into_iter()),
                    };
                ids.filter(|&id| s.is_live(id))
                    .filter_map(|id| {
                        let p = s.seg.path(id);
                        compiled
                            .path_ok(p)
                            .then(|| (0u32, String::from_utf8_lossy(p).into_owned()))
                    })
                    .collect()
            })
            .collect()
    });
    let cands: Vec<(u32, String)> = per_seg.into_iter().flatten().collect();
    run(
        root, cands, &compiled, opts, &pool, &mut stats, t0, false, emit,
    );
    Ok(stats)
}

/// Answers a text query with no index, the way ripgrep would: walk the tree
/// with the same ignore rules and skip rules the indexer uses, then search
/// every file. Used while the first index builds in the background.
/// `sym:` and `calls:` need the index and fail here.
pub fn search_unindexed(
    root: &Path,
    query: &str,
    opts: &SearchOptions,
    emit: &mut dyn FnMut(&Hit) -> bool,
) -> Result<SearchStats> {
    let t0 = Instant::now();
    let parsed = parse(query, opts)?;
    if parsed.sym.is_some() {
        bail!("sym: and calls: need the index, which is still building; try again shortly, or search the text");
    }
    let compiled = compile_parsed(&parsed, opts)?;
    let mut stats = SearchStats {
        plan: "scan (no index yet)".to_string(),
        plan_time: t0.elapsed(),
        ..Default::default()
    };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(opts.threads.max(1))
        .build()?;
    let paths = walk(root, opts.threads)?;
    let cands: Vec<(u32, String)> = paths
        .iter()
        .filter(|p| compiled.path_ok(p.as_bytes()))
        .map(|p| (0u32, p.clone()))
        .collect();
    stats.live_files = paths.len() as u64;
    run(
        root, cands, &compiled, opts, &pool, &mut stats, t0, true, emit,
    );
    Ok(stats)
}

/// Every file the indexer would index, relative to `root`.
pub fn walk(root: &Path, threads: usize) -> Result<Vec<String>> {
    let rules = scout_index::Rules::new(root, &[])?;
    let skipped = std::sync::atomic::AtomicU64::new(0);
    let paths = scout_index::build::walk(
        &rules,
        threads.max(1),
        scout_index::filter::DEFAULT_MAX_FILE_SIZE,
        &skipped,
    );
    Ok(paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect())
}

/// Sort key for file hits: a match in the base name first, then the usual
/// path rank, then shorter paths.
fn file_rank(c: &Compiled, path: &str, near: Option<&str>) -> (u32, u32, usize) {
    let base = path.rsplit('/').next().unwrap_or(path).as_bytes();
    let in_base = c.positive_path.iter().any(|&i| {
        c.terms[i].kind == TermKind::Text
            && c.terms[i]
                .path
                .as_ref()
                .is_some_and(|m| m.is_match(base).unwrap_or(false))
    });
    (u32::from(!in_base), path_rank(path, near), path.len())
}

struct FileResult {
    hits: Vec<Hit>,
    searched: bool,
}

#[allow(clippy::too_many_arguments)]
fn run(
    root: &Path,
    mut cands: Vec<(u32, String)>,
    c: &Compiled,
    opts: &SearchOptions,
    pool: &rayon::ThreadPool,
    stats: &mut SearchStats,
    t0: Instant,
    read_whole: bool,
    emit: &mut dyn FnMut(&Hit) -> bool,
) {
    stats.candidates = cands.len();
    rank(&mut cands, opts.near.as_deref());
    stats.candidate_time = t0.elapsed().saturating_sub(stats.plan_time);
    let max = opts.max_matches.max(1);
    let lines_wanted = !c.positive_content.is_empty();
    let mut matched: HashSet<String> = HashSet::new();
    let mut stop = false;

    // Shown hits, in order; false once the caller or the cap says stop.
    let mut push = |h: &Hit, stats: &mut SearchStats, matched: &mut HashSet<String>| -> bool {
        if stats.matches >= max {
            stats.truncated = true;
            return false;
        }
        stats.first_hit.get_or_insert_with(|| t0.elapsed());
        stats.matches += 1;
        if h.is_file() {
            stats.file_hits += 1;
        }
        if !matched.contains(&h.path) {
            matched.insert(h.path.clone());
        }
        emit(h)
    };

    // 1. File-name hits.
    let file_cap = if lines_wanted {
        opts.max_file_hits.min(max)
    } else {
        max
    };
    let want_files = (opts.paths || !lines_wanted) && !c.positive_path.is_empty() && file_cap > 0;
    if want_files {
        let mut fc: Vec<((u32, u32, usize), &str)> = cands
            .iter()
            .filter(|(_, p)| {
                let pm = c.path_matches(p.as_bytes());
                c.path_eval(&c.expr, &pm, &|e| c.eval3(e, &pm).unwrap_or(false))
            })
            .map(|(_, p)| (file_rank(c, p, opts.near.as_deref()), p.as_str()))
            .collect();
        fc.sort();
        let mut shown = 0usize;
        let mut start = 0;
        let mut chunk = 32;
        'files: while start < fc.len() && shown < file_cap {
            let end = (start + chunk).min(fc.len());
            let res: Vec<Option<Hit>> = pool.install(|| {
                fc[start..end]
                    .par_iter()
                    .map(|(_, p)| {
                        let pm = c.path_matches(p.as_bytes());
                        if c.neg_content {
                            // `-term` on contents: read the file to check.
                            let data = read(root, p, read_whole)?;
                            let vals = content_vals(c, p, &data, &pm);
                            if !c.path_eval(&c.expr, &pm, &|e| c.eval(e, &|i| vals[i])) {
                                return None;
                            }
                        }
                        let mut ranges = Vec::new();
                        for &i in &c.positive_path {
                            if let Some(m) = &c.terms[i].path {
                                let _ = m.find_iter(p.as_bytes(), |mt| {
                                    ranges.push((mt.start(), mt.end()));
                                    true
                                });
                            }
                        }
                        Some(Hit {
                            path: p.to_string(),
                            line: 1,
                            col: 1,
                            text: Vec::new(),
                            ranges: merge(ranges),
                            kind: Some("file"),
                            symbol: None,
                            container: None,
                        })
                    })
                    .collect()
            });
            for h in res.into_iter().flatten() {
                if shown >= file_cap {
                    if !lines_wanted {
                        stats.truncated = true;
                    }
                    break 'files;
                }
                shown += 1;
                if !push(&h, stats, &mut matched) {
                    stop = true;
                    break 'files;
                }
            }
            start = end;
            chunk = (chunk * 2).min(1024);
        }
        if !lines_wanted && start < fc.len() && shown >= file_cap {
            stats.truncated = true;
        }
    }

    // 2. Line hits from files that satisfy the query.
    if lines_wanted && !stop {
        let halt = AtomicBool::new(false);
        let mut start = 0usize;
        let mut chunk = 32usize;
        while start < cands.len() && !halt.load(Ordering::Relaxed) {
            let end = (start + chunk).min(cands.len());
            let remaining = max.saturating_sub(stats.matches).max(1);
            let results: Vec<FileResult> = pool.install(|| {
                cands[start..end]
                    .par_iter()
                    .map(|(_, rel)| {
                        if halt.load(Ordering::Relaxed) {
                            return FileResult {
                                hits: Vec::new(),
                                searched: false,
                            };
                        }
                        FileResult {
                            hits: file_lines(root, rel, c, opts, remaining, read_whole),
                            searched: true,
                        }
                    })
                    .collect()
            });
            for r in results {
                if r.searched {
                    stats.files_searched += 1;
                }
                for h in &r.hits {
                    if !push(h, stats, &mut matched) {
                        halt.store(true, Ordering::Relaxed);
                        break;
                    }
                }
                if halt.load(Ordering::Relaxed) {
                    break;
                }
            }
            start = end;
            chunk = (chunk * 2).min(1024);
        }
    }
    stats.files_matched = matched.len();
    stats.total_time = t0.elapsed();
}

fn read(root: &Path, rel: &str, read_whole: bool) -> Option<Vec<u8>> {
    let data = std::fs::read(root.join(rel)).ok()?;
    if read_whole && scout_index::filter::check_content(&data).is_some() {
        return None;
    }
    Some(data)
}

/// Sorted ranges with overlaps dropped.
fn merge(mut r: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    r.sort();
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(r.len());
    for x in r {
        if out.last().is_none_or(|l| x.0 >= l.1) {
            out.push(x);
        }
    }
    out
}

/// Runs one term over a file's contents.
fn term_hits(
    c: &Compiled,
    i: usize,
    rel: &str,
    data: &[u8],
    regions: Option<&Regions>,
    max_line: usize,
    limit: usize,
) -> Vec<Hit> {
    let t = &c.terms[i];
    let Some(m) = &t.matcher else {
        return Vec::new();
    };
    let mut sink = HitSink {
        matcher: m,
        hits: Vec::new(),
        path: rel,
        max_line,
        limit: limit.max(1),
        kind: t.kind,
        regions,
    };
    let _ = searcher(c.pcre).search_slice(m, data, &mut sink);
    sink.hits
}

/// Per-term truth values for a file: path or contents.
fn content_vals(c: &Compiled, rel: &str, data: &[u8], pm: &[bool]) -> Vec<bool> {
    let regions = c.needs_regions.then(|| regions::regions(rel, data));
    (0..c.terms.len())
        .map(|i| {
            let t = &c.terms[i];
            match t.kind {
                TermKind::Path => pm[i],
                TermKind::Text if pm[i] => true,
                _ => !term_hits(c, i, rel, data, regions.as_ref(), 1, 1).is_empty(),
            }
        })
        .collect()
}

/// Line hits for one file, or none when the file doesn't satisfy the query.
fn file_lines(
    root: &Path,
    rel: &str,
    c: &Compiled,
    opts: &SearchOptions,
    limit: usize,
    read_whole: bool,
) -> Vec<Hit> {
    let pm = c.path_matches(rel.as_bytes());
    if c.eval3(&c.expr, &pm) == Some(false) {
        return Vec::new();
    }
    let Some(data) = read(root, rel, read_whole) else {
        return Vec::new();
    };
    let regions = c.needs_regions.then(|| regions::regions(rel, &data));
    let mut per_term: Vec<Option<Vec<Hit>>> = vec![None; c.terms.len()];
    for &i in &c.positive_content {
        per_term[i] = Some(term_hits(
            c,
            i,
            rel,
            &data,
            regions.as_ref(),
            opts.max_line_bytes,
            limit,
        ));
    }
    let vals: Vec<bool> = (0..c.terms.len())
        .map(|i| {
            let t = &c.terms[i];
            match (&per_term[i], t.kind) {
                (_, TermKind::Path) => pm[i],
                (Some(h), k) => !h.is_empty() || (k == TermKind::Text && pm[i]),
                (None, TermKind::Text) if pm[i] => true,
                (None, _) => !term_hits(c, i, rel, &data, regions.as_ref(), 1, 1).is_empty(),
            }
        })
        .collect();
    if !c.eval(&c.expr, &|i| vals[i]) {
        return Vec::new();
    }
    let mut by_line: BTreeMap<u64, Hit> = BTreeMap::new();
    for h in per_term.into_iter().flatten().flatten() {
        match by_line.get_mut(&h.line) {
            Some(x) => x.ranges.extend(h.ranges),
            None => {
                by_line.insert(h.line, h);
            }
        }
    }
    let mut out: Vec<Hit> = by_line.into_values().collect();
    for h in out.iter_mut() {
        h.ranges = merge(std::mem::take(&mut h.ranges));
        h.col = h.ranges.first().map(|r| r.0 as u64 + 1).unwrap_or(1);
    }
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests;
