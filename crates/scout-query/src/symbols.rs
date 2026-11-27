//! `sym:` and `calls:` search over the symbol sidecars.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use rayon::prelude::*;

use scout_index::symbols::Kind;
use scout_index::Index;

use crate::filters::{Parsed, SymMode};
use crate::{has_upper, Case, Compiled, Hit, SearchOptions, SearchStats};

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

fn parse_kinds(names: &[String]) -> Result<Vec<Kind>> {
    let mut out = Vec::new();
    for n in names {
        let n = n.to_ascii_lowercase();
        let ks: &[Kind] = match n.as_str() {
            "func" | "fn" | "function" | "functions" => &[Kind::Function, Kind::Method],
            "method" | "methods" => &[Kind::Method],
            "class" | "classes" => &[Kind::Class, Kind::Struct, Kind::Interface, Kind::Trait],
            "type" | "types" => &[
                Kind::Class,
                Kind::Struct,
                Kind::Enum,
                Kind::Union,
                Kind::Type,
                Kind::Interface,
                Kind::Trait,
            ],
            "const" | "constant" | "enumerator" => &[Kind::Constant],
            "var" | "variable" => &[Kind::Variable],
            "decl" | "prototype" | "declaration" => &[Kind::Prototype],
            "rule" | "target" => &[Kind::Target],
            other => match Kind::from_name(other) {
                Some(k) if k != Kind::Call => {
                    out.push(k);
                    continue;
                }
                _ => bail!(
                    "unknown kind `{}` (try function, method, class, struct, enum, type, macro, constant, target)",
                    n
                ),
            },
        };
        out.extend_from_slice(ks);
    }
    Ok(out)
}

/// Lower sorts first: real definitions, then constants, then prototypes.
fn kind_rank(k: Kind) -> u32 {
    match k {
        Kind::Prototype => 2,
        Kind::Constant | Kind::Variable => 1,
        _ => 0,
    }
}

struct Found {
    score: (u32, u32, u32),
    path: String,
    line: u32,
    kind: Kind,
    name: String,
    container: Option<String>,
}

/// Runs a `sym:`/`calls:` query. Results come from the index; each line is
/// read back from the working tree, and a hit whose name has moved more
/// than a few lines (the file changed since it was indexed) is dropped.
pub(crate) fn search(
    index: &Index,
    root: &Path,
    parsed: &Parsed,
    compiled: &Compiled,
    opts: &SearchOptions,
    emit: &mut dyn FnMut(&Hit) -> bool,
) -> Result<SearchStats> {
    let t0 = Instant::now();
    let mode = parsed.sym.expect("symbol query");
    if !index.manifest.symbols {
        bail!("this index has no symbols; rebuild it with `scout build` (or :ScoutIndex rebuild)");
    }
    let mut stats = SearchStats {
        segments: index.segments.len(),
        live_files: index.live_count(),
        ..Default::default()
    };
    let pending = index.segments.iter().filter(|s| s.syms.is_none()).count();
    if pending > 0 {
        stats.symbols_incomplete = true;
    }
    let name = parsed.name.as_str();
    if name.is_empty() {
        bail!("sym: and calls: need a name");
    }
    let kinds = parse_kinds(&parsed.kinds)?;
    let case = parsed.case.unwrap_or(opts.case);
    let insensitive = match case {
        Case::Sensitive => false,
        Case::Insensitive => true,
        Case::Smart => !has_upper(name),
    };
    // `calls:foo` means exactly foo; `sym:foo` also finds names containing
    // foo, best matches first. Anything with regex syntax is a regex.
    let literal = is_ident(name);
    let exact_only = literal && mode == SymMode::Calls;
    let name_re: Option<RegexMatcher> = if exact_only {
        None
    } else {
        let pat = if literal {
            regex_syntax::escape(name)
        } else {
            name.to_string()
        };
        Some(
            RegexMatcherBuilder::new()
                .case_insensitive(insensitive)
                .build(&pat)
                .with_context(|| format!("invalid name regex `{}`", name))?,
        )
    };
    let line_filter = compiled.has_line_filter();
    stats.plan = format!(
        "{} {}",
        if mode == SymMode::Defs {
            "sym"
        } else {
            "calls"
        },
        name
    );
    stats.plan_time = t0.elapsed();

    let want_calls = mode == SymMode::Calls;
    let lower = name.to_lowercase();
    let near = opts.near.as_deref();
    let cap = 200_000usize;
    let per_seg: Vec<Vec<Found>> = index
        .segments
        .par_iter()
        .map(|s| {
            let mut out = Vec::new();
            let Some(sf) = s.syms.as_ref() else {
                return out;
            };
            let visit = |nid: u32, out: &mut Vec<Found>| {
                let n = String::from_utf8_lossy(sf.name(nid));
                let class = if n == name {
                    0
                } else if n.to_lowercase() == lower {
                    1
                } else if n.to_lowercase().starts_with(&lower) {
                    2
                } else {
                    3
                };
                for r in sf.records(nid) {
                    if (r.kind == Kind::Call) != want_calls || !s.is_live(r.file) {
                        continue;
                    }
                    if !kinds.is_empty() && !kinds.contains(&r.kind) {
                        continue;
                    }
                    let p = s.seg.path(r.file);
                    if !compiled.sym_path_ok(p) {
                        continue;
                    }
                    let path = String::from_utf8_lossy(p).into_owned();
                    let prank = crate::path_rank(&path, near);
                    out.push(Found {
                        score: (class, kind_rank(r.kind), prank),
                        path,
                        line: r.line,
                        kind: r.kind,
                        name: n.clone().into_owned(),
                        container: r
                            .container
                            .map(|c| String::from_utf8_lossy(sf.name(c)).into_owned()),
                    });
                    if out.len() >= cap {
                        break;
                    }
                }
            };
            match &name_re {
                None => {
                    if let Some(nid) = sf.find(name.as_bytes()) {
                        visit(nid, &mut out);
                    }
                }
                Some(re) => {
                    for nid in 0..sf.num_names() {
                        if re.is_match(sf.name(nid)).unwrap_or(false) {
                            visit(nid, &mut out);
                            if out.len() >= cap {
                                break;
                            }
                        }
                    }
                }
            }
            out
        })
        .collect();
    let mut found: Vec<Found> = per_seg.into_iter().flatten().collect();
    stats.candidates = found.len();
    found.sort_by(|a, b| {
        a.score
            .cmp(&b.score)
            .then_with(|| a.path.cmp(&b.path))
            .then(a.line.cmp(&b.line))
    });
    stats.candidate_time = t0.elapsed() - stats.plan_time;

    // Resolve line text in rank order, a chunk of files at a time.
    let max = opts.max_matches.max(1);
    let mut start = 0;
    let mut chunk = 64;
    let mut stop = false;
    let mut matched_files: std::collections::HashSet<String> = Default::default();
    while start < found.len() && !stop {
        let end = (start + chunk).min(found.len());
        let mut files: Vec<&str> = found[start..end].iter().map(|f| f.path.as_str()).collect();
        files.sort_unstable();
        files.dedup();
        let texts: HashMap<&str, Option<Vec<u8>>> = files
            .par_iter()
            .map(|&p| (p, fs::read(root.join(p)).ok()))
            .collect();
        stats.files_searched += texts.len();
        for f in &found[start..end] {
            let Some(Some(data)) = texts.get(f.path.as_str()) else {
                continue;
            };
            let Some((line, text, col)) = locate(data, f.line, &f.name) else {
                continue;
            };
            if line_filter && !compiled.sym_line_ok(&text) {
                continue;
            }
            if stats.matches >= max {
                stats.truncated = true;
                stop = true;
                break;
            }
            let text_cut = if text.len() > opts.max_line_bytes {
                text[..opts.max_line_bytes].to_vec()
            } else {
                text
            };
            let ranges = if col + f.name.len() <= text_cut.len() {
                vec![(col, col + f.name.len())]
            } else {
                Vec::new()
            };
            let hit = Hit {
                path: f.path.clone(),
                line: line as u64,
                col: col as u64 + 1,
                text: text_cut,
                ranges,
                kind: Some(f.kind.name()),
                symbol: Some(f.name.clone()),
                container: f.container.clone(),
            };
            stats.first_hit.get_or_insert_with(|| t0.elapsed());
            stats.matches += 1;
            matched_files.insert(f.path.clone());
            if !emit(&hit) {
                stop = true;
                break;
            }
        }
        start = end;
        chunk = (chunk * 2).min(2048);
    }
    stats.files_matched = matched_files.len();
    stats.total_time = t0.elapsed();
    Ok(stats)
}

/// Finds `name` as a whole word on `line` (1-based) or within a few lines
/// of it. Returns the line it was found on, that line's text and the byte
/// column of the name.
fn locate(data: &[u8], line: u32, name: &str) -> Option<(u32, Vec<u8>, usize)> {
    let lines: Vec<&[u8]> = data.split(|&b| b == b'\n').collect();
    let want = line as i64 - 1;
    for d in [0i64, -1, 1, -2, 2, -3, 3, -5, 5, -8, 8] {
        let i = want + d;
        if i < 0 || i as usize >= lines.len() {
            continue;
        }
        let mut l = lines[i as usize];
        if l.last() == Some(&b'\r') {
            l = &l[..l.len() - 1];
        }
        if let Some(c) = find_word(l, name.as_bytes()) {
            return Some((i as u32 + 1, l.to_vec(), c));
        }
    }
    None
}

fn find_word(hay: &[u8], w: &[u8]) -> Option<usize> {
    if w.is_empty() || hay.len() < w.len() {
        return None;
    }
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80;
    let mut i = 0;
    while i + w.len() <= hay.len() {
        if &hay[i..i + w.len()] == w
            && (i == 0 || !is_word(hay[i - 1]))
            && (i + w.len() == hay.len() || !is_word(hay[i + w.len()]))
        {
            return Some(i);
        }
        i += 1;
    }
    None
}
