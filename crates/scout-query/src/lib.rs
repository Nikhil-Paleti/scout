//! Query execution: parse, plan, collect candidates per segment, verify
//! against the working tree.

pub mod eval;
pub mod filters;
pub mod plan;
mod symbols;

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, MmapChoice, Searcher, SearcherBuilder, Sink, SinkMatch};
use rayon::prelude::*;

use scout_index::Index;

pub use filters::Case;
use eval::Cand;
use plan::Q;

#[derive(Clone, Debug)]
pub struct SearchOptions {
    pub case: Case,
    pub fixed: bool,
    pub word: bool,
    pub max_matches: usize,
    pub threads: usize,
    /// Path (relative to the root) of the file the user is in, for ranking.
    pub near: Option<String>,
    /// Parse `file:`/`lang:`/`case:` filters out of the query.
    pub filters: bool,
    /// Cap on bytes of line text returned per hit.
    pub max_line_bytes: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        SearchOptions {
            case: Case::Smart,
            fixed: false,
            word: false,
            max_matches: 5000,
            threads: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4),
            near: None,
            filters: true,
            max_line_bytes: 2000,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Hit {
    pub path: String,
    pub line: u64,
    /// 1-based byte column of the first match.
    pub col: u64,
    pub text: Vec<u8>,
    /// Byte ranges of every match within `text`.
    pub ranges: Vec<(usize, usize)>,
    /// For symbol hits: the kind (function, call, ...), the symbol's name
    /// and, for calls, the function the call is in.
    pub kind: Option<&'static str>,
    pub symbol: Option<String>,
    pub container: Option<String>,
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
    pub truncated: bool,
    pub plan_time: Duration,
    pub candidate_time: Duration,
    pub total_time: Duration,
    pub first_hit: Option<Duration>,
    /// Symbol search ran while some segments still lack symbols.
    pub symbols_incomplete: bool,
}

/// A compiled query: the matcher for verification plus the trigram plan.
pub struct Compiled {
    pub matcher: RegexMatcher,
    pub plan: Q,
    file: Vec<RegexMatcher>,
    not_file: Vec<RegexMatcher>,
}

fn has_upper(s: &str) -> bool {
    // Mirrors ripgrep's smart case closely enough: an uppercase literal
    // character anywhere (escapes like \W or \S also count; that only makes
    // the search case sensitive, never wrong).
    s.chars().any(|c| c.is_uppercase())
}

pub fn parse(query: &str, opts: &SearchOptions) -> Result<filters::Parsed> {
    Ok(if opts.filters {
        filters::parse(query)?
    } else {
        filters::Parsed { pattern: query.to_string(), ..Default::default() }
    })
}

pub fn compile(query: &str, opts: &SearchOptions) -> Result<Compiled> {
    compile_parsed(&parse(query, opts)?, opts)
}

pub fn compile_parsed(parsed: &filters::Parsed, opts: &SearchOptions) -> Result<Compiled> {
    let case = parsed.case.unwrap_or(opts.case);
    let insensitive = match case {
        Case::Sensitive => false,
        Case::Insensitive => true,
        Case::Smart => !has_upper(&parsed.pattern),
    };
    let mut pattern = if opts.fixed { regex_syntax::escape(&parsed.pattern) } else { parsed.pattern.clone() };
    if opts.word {
        pattern = format!(r"\b(?:{})\b", pattern);
    }
    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(insensitive)
        .line_terminator(Some(b'\n'))
        .build(&pattern)
        .with_context(|| format!("invalid regex `{}`", pattern))?;
    let hir = regex_syntax::ParserBuilder::new()
        .case_insensitive(insensitive)
        .utf8(false)
        .build()
        .parse(&pattern)
        .with_context(|| format!("invalid regex `{}`", pattern))?;
    let plan = plan::plan(&hir);
    let path_matcher = |re: &String| -> Result<RegexMatcher> {
        RegexMatcherBuilder::new()
            .case_smart(true)
            .build(re)
            .with_context(|| format!("invalid file filter `{}`", re))
    };
    Ok(Compiled {
        matcher,
        plan,
        file: parsed.file.iter().map(path_matcher).collect::<Result<_>>()?,
        not_file: parsed.not_file.iter().map(path_matcher).collect::<Result<_>>()?,
    })
}

impl Compiled {
    fn path_ok(&self, path: &[u8]) -> bool {
        self.file.iter().all(|m| m.is_match(path).unwrap_or(false))
            && !self.not_file.iter().any(|m| m.is_match(path).unwrap_or(false))
    }
}

fn is_lowrank(path: &str) -> u32 {
    let mut p = 0;
    for part in path.split('/') {
        match part {
            "vendor" | "third_party" | "thirdparty" | "external" | "node_modules" | "deps" => p += 4,
            "test" | "tests" | "testdata" | "testing" | "__tests__" | "spec" => p += 2,
            _ => {}
        }
    }
    let file = path.rsplit('/').next().unwrap_or(path);
    if file.contains("_test.") || file.contains(".test.") || file.contains("_spec.") || file.starts_with("test_") {
        p += 2;
    }
    p
}

fn shared_dirs(a: &str, near: &str) -> u32 {
    let ad = a.rsplit_once('/').map(|x| x.0).unwrap_or("");
    let nd = near.rsplit_once('/').map(|x| x.0).unwrap_or("");
    ad.split('/').zip(nd.split('/')).take_while(|(x, y)| x == y && !x.is_empty()).count() as u32
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
}

impl Sink for HitSink<'_> {
    type Error = io::Error;

    fn matched(&mut self, _s: &Searcher, m: &SinkMatch<'_>) -> Result<bool, io::Error> {
        let mut line = m.bytes();
        while let Some((&last, rest)) = line.split_last() {
            if last == b'\n' || last == b'\r' {
                line = rest;
            } else {
                break;
            }
        }
        let mut ranges = Vec::new();
        let _ = self.matcher.find_iter(line, |mt| {
            ranges.push((mt.start(), mt.end()));
            ranges.len() < 64
        });
        let col = ranges.first().map(|r| r.0 as u64 + 1).unwrap_or(1);
        let text = if line.len() > self.max_line { line[..self.max_line].to_vec() } else { line.to_vec() };
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

fn searcher() -> Searcher {
    SearcherBuilder::new()
        .line_number(true)
        .binary_detection(BinaryDetection::quit(0))
        // Candidates are mostly small source files, where read() beats
        // setting up a mapping per file (ripgrep makes the same call).
        .memory_map(MmapChoice::never())
        .build()
}

/// Runs `query` against `index`, calling `emit` for each hit in rank order
/// until it returns false or the match cap is reached.
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
        plan: compiled.plan.describe(),
        segments: index.segments.len(),
        live_files: index.live_count(),
        plan_time: t0.elapsed(),
        ..Default::default()
    };

    let pool = rayon::ThreadPoolBuilder::new().num_threads(opts.threads.max(1)).build()?;
    // Candidate collection, segment by segment.
    let per_seg: Vec<Vec<(u32, String)>> = pool.install(|| {
        index
            .segments
            .par_iter()
            .map(|s| {
                let ids: Box<dyn Iterator<Item = u32>> = match eval::eval(&s.seg, &compiled.plan) {
                    Cand::All => Box::new(0..s.seg.num_files()),
                    Cand::Ids(v) => Box::new(v.into_iter()),
                };
                ids.filter(|&id| s.is_live(id))
                    .filter_map(|id| {
                        let p = s.seg.path(id);
                        if compiled.path_ok(p) {
                            Some((0u32, String::from_utf8_lossy(p).into_owned()))
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .collect()
    });
    let mut cands: Vec<(u32, String)> = per_seg.into_iter().flatten().collect();
    stats.candidates = cands.len();
    rank(&mut cands, opts.near.as_deref());
    stats.candidate_time = t0.elapsed() - stats.plan_time;

    let stop = AtomicBool::new(false);
    let max = opts.max_matches.max(1);
    let mut start = 0usize;
    let mut chunk = 32usize;
    while start < cands.len() && !stop.load(Ordering::Relaxed) {
        let end = (start + chunk).min(cands.len());
        let remaining = max - stats.matches;
        let results: Vec<Vec<Hit>> = pool.install(|| {
            cands[start..end]
                .par_iter()
                .map_init(searcher, |srch, (_, rel)| {
                    if stop.load(Ordering::Relaxed) {
                        return Vec::new();
                    }
                    let mut sink = HitSink {
                        matcher: &compiled.matcher,
                        hits: Vec::new(),
                        path: rel,
                        max_line: opts.max_line_bytes,
                        limit: remaining,
                    };
                    let _ = srch.search_path(&compiled.matcher, root.join(rel), &mut sink);
                    sink.hits
                })
                .collect()
        });
        stats.files_searched += end - start;
        for hits in results {
            if hits.is_empty() {
                continue;
            }
            stats.files_matched += 1;
            for h in &hits {
                if stats.matches >= max {
                    stats.truncated = true;
                    stop.store(true, Ordering::Relaxed);
                    break;
                }
                if stats.first_hit.is_none() {
                    stats.first_hit = Some(t0.elapsed());
                }
                stats.matches += 1;
                if !emit(h) {
                    stop.store(true, Ordering::Relaxed);
                    break;
                }
            }
            if stop.load(Ordering::Relaxed) {
                break;
            }
        }
        start = end;
        chunk = (chunk * 2).min(1024);
    }
    stats.total_time = t0.elapsed();
    Ok(stats)
}

#[cfg(test)]
mod tests;
