//! `scout files QUERY`: file-name search. A query with regex syntax is a
//! regex over the path; otherwise each word is a fuzzy subsequence (fzf
//! style), scored for consecutive letters, word starts and hits in the base
//! name. A `-term` (or `-file:term`) word drops paths that match `term` as a
//! case-insensitive regex, as it does in a code search.
//!
//! `scout dirs QUERY` runs the same matcher over folders: every parent
//! directory of an indexed path, so it needs no index of its own.

use anyhow::{Context, Result};
use grep_matcher::Matcher;
use grep_regex::RegexMatcherBuilder;
use rayon::prelude::*;

#[derive(Clone, Debug)]
pub struct FileMatch {
    pub path: String,
    pub score: i64,
    /// Byte offsets of the matched characters in `path`.
    pub positions: Vec<usize>,
}

/// Whether `q` reads as a regex rather than fuzzy words. A lone `.` is
/// common in file names, so it doesn't count.
pub fn is_regex(q: &str) -> bool {
    q.chars().any(|c| {
        matches!(
            c,
            '\\' | '^' | '$' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|'
        )
    })
}

fn boundary(prev: u8, cur: u8) -> i64 {
    match prev {
        b'/' => 28,
        b'_' | b'-' | b'.' | b' ' => 20,
        p if p.is_ascii_lowercase() && cur.is_ascii_uppercase() => 20,
        p if !p.is_ascii_digit() && cur.is_ascii_digit() => 8,
        _ => 0,
    }
}

/// Scores matched positions within `path`.
fn score_positions(path: &[u8], pos: &[usize], base_start: usize) -> i64 {
    let mut s = 0i64;
    let mut prev: Option<usize> = None;
    for &p in pos {
        s += 16;
        if p == 0 {
            s += 28;
        } else {
            s += boundary(path[p - 1], path[p]);
        }
        if p >= base_start {
            s += 10;
        }
        match prev {
            Some(q) if q + 1 == p => s += 24,
            Some(q) => s -= 3 + (p - q - 1).min(20) as i64,
            None => {}
        }
        prev = Some(p);
    }
    s
}

/// fzf v1: leftmost forward match to find where the word ends, then
/// backward from there for the tightest window.
fn fuzzy_in(hay: &[u8], word: &[u8], from: usize, sensitive: bool) -> Option<Vec<usize>> {
    let eq = |a: u8, b: u8| {
        if sensitive {
            a == b
        } else {
            a.eq_ignore_ascii_case(&b)
        }
    };
    let mut wi = 0;
    let mut end = None;
    for (i, &b) in hay.iter().enumerate().skip(from) {
        if eq(b, word[wi]) {
            wi += 1;
            if wi == word.len() {
                end = Some(i);
                break;
            }
        }
    }
    let end = end?;
    let mut pos = vec![0; word.len()];
    let mut wi = word.len();
    let mut i = end + 1;
    while wi > 0 {
        i -= 1;
        if eq(hay[i], word[wi - 1]) {
            wi -= 1;
            pos[wi] = i;
        }
    }
    Some(pos)
}

fn fuzzy_word(path: &[u8], word: &str, base_start: usize) -> Option<(i64, Vec<usize>)> {
    let w = word.as_bytes();
    let sensitive = word.chars().any(|c| c.is_uppercase());
    let mut best: Option<(i64, Vec<usize>)> = None;
    // Try the base name first; a hit there usually beats one spread over
    // directories.
    for from in [base_start, 0] {
        if let Some(pos) = fuzzy_in(path, w, from, sensitive) {
            let mut s = score_positions(path, &pos, base_start);
            if pos[0] == base_start && pos.windows(2).all(|x| x[1] == x[0] + 1) {
                s += 40;
                // The whole stem (`dev` for dev.c) beats a prefix of it.
                let stem_end = path[base_start..]
                    .iter()
                    .position(|&b| b == b'.')
                    .map(|i| base_start + i)
                    .unwrap_or(path.len());
                if pos.last().map(|&l| l + 1) == Some(stem_end) {
                    s += 30;
                }
            }
            if best.as_ref().is_none_or(|b| s > b.0) {
                best = Some((s, pos));
            }
        }
    }
    best
}

/// Scores one path against fuzzy words; `None` if any word is missing.
pub fn fuzzy(path: &str, words: &[&str]) -> Option<(i64, Vec<usize>)> {
    let p = path.as_bytes();
    let base_start = path.rfind('/').map(|i| i + 1).unwrap_or(0);
    let mut total = 0;
    let mut all = Vec::new();
    for w in words {
        let (s, pos) = fuzzy_word(p, w, base_start)?;
        total += s;
        all.extend(pos);
    }
    all.sort_unstable();
    all.dedup();
    Some((total, all))
}

/// Splits `-term` words off a query: the rest of the query, and a matcher
/// for paths to drop.
fn exclusions(query: &str) -> Result<(String, Option<grep_regex::RegexMatcher>)> {
    let mut keep = Vec::new();
    let mut drop = Vec::new();
    for w in query.split_whitespace() {
        match w.strip_prefix('-') {
            Some(t) if !t.is_empty() => {
                let t = t.strip_prefix("file:").unwrap_or(t);
                if !t.is_empty() {
                    drop.push(t);
                }
            }
            _ => keep.push(w),
        }
    }
    if drop.is_empty() {
        return Ok((query.to_string(), None));
    }
    let alts: Vec<String> = drop
        .iter()
        .map(|t| {
            if regex_ok(t) {
                format!("(?:{})", t)
            } else {
                regex_escape(t)
            }
        })
        .collect();
    let m = RegexMatcherBuilder::new()
        .case_insensitive(true)
        .build(&alts.join("|"))
        .with_context(|| format!("invalid exclusion in `{}`", query))?;
    Ok((keep.join(" "), Some(m)))
}

fn regex_ok(t: &str) -> bool {
    RegexMatcherBuilder::new().build(t).is_ok()
}

fn regex_escape(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    for c in t.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Every folder that holds an indexed path, relative to the root, sorted.
/// The root itself is left out. Stops walking up once a parent is known,
/// so it is about one hash lookup per file.
pub fn dirs(paths: &[String]) -> Vec<String> {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for p in paths {
        let mut cur = p.as_str();
        while let Some(i) = cur.rfind('/') {
            cur = &cur[..i];
            if cur.is_empty() || !seen.insert(cur) {
                break;
            }
        }
    }
    let mut out: Vec<String> = seen.into_iter().map(str::to_string).collect();
    out.sort_unstable();
    out
}

/// Matches `paths` against `query`, best first, at most `limit`.
pub fn find(
    paths: &[String],
    query: &str,
    near: Option<&str>,
    limit: usize,
) -> Result<Vec<FileMatch>> {
    let (rest, drop) = exclusions(query)?;
    let kept: Vec<String>;
    let paths: &[String] = match &drop {
        Some(m) => {
            kept = paths
                .iter()
                .filter(|p| !m.is_match(p.as_bytes()).unwrap_or(false))
                .cloned()
                .collect();
            &kept
        }
        None => paths,
    };
    let q = rest.trim();
    let mut out: Vec<FileMatch> = if q.is_empty() {
        paths
            .iter()
            .map(|p| FileMatch {
                path: p.clone(),
                score: 0,
                positions: Vec::new(),
            })
            .collect()
    } else if is_regex(q) {
        let m = RegexMatcherBuilder::new()
            .case_smart(true)
            .build(q)
            .with_context(|| format!("invalid regex `{}`", q))?;
        paths
            .par_iter()
            .filter_map(|p| {
                let mt = m.find(p.as_bytes()).ok().flatten()?;
                let base_start = p.rfind('/').map(|i| i + 1).unwrap_or(0);
                let mut score = 100 - p.len() as i64 / 2;
                if mt.start() >= base_start {
                    score += 50;
                }
                Some(FileMatch {
                    path: p.clone(),
                    score,
                    positions: (mt.start()..mt.end()).collect(),
                })
            })
            .collect()
    } else {
        let words: Vec<&str> = q.split_whitespace().collect();
        paths
            .par_iter()
            .filter_map(|p| {
                let (s, pos) = fuzzy(p, &words)?;
                Some(FileMatch {
                    path: p.clone(),
                    score: s - p.len() as i64 / 4,
                    positions: pos,
                })
            })
            .collect()
    };
    if !q.is_empty() {
        for m in out.iter_mut() {
            m.score -= (crate::path_rank(&m.path, near) as i64 - 1000) * 2;
        }
    }
    out.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(a.path.len().cmp(&b.path.len()))
            .then_with(|| a.path.cmp(&b.path))
    });
    if q.is_empty() {
        out.sort_by(|a, b| a.path.cmp(&b.path));
    }
    out.truncate(limit);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn top(paths: &[&str], q: &str) -> Vec<String> {
        let v: Vec<String> = paths.iter().map(|s| s.to_string()).collect();
        find(&v, q, None, 10)
            .unwrap()
            .into_iter()
            .map(|m| m.path)
            .collect()
    }

    #[test]
    fn fuzzy_prefers_base_names_and_word_starts() {
        let paths = [
            "src/main.rs",
            "src/domain/info.rs",
            "lib/mail/main_test.go",
            "vendor/x/main.rs",
            "docs/readme.md",
        ];
        assert_eq!(top(&paths, "main")[0], "src/main.rs");
        assert_eq!(top(&paths, "mainrs")[0], "src/main.rs");
        assert_eq!(top(&paths, "mt"), vec!["lib/mail/main_test.go"]);
        assert!(top(&paths, "zzz").is_empty());
        // Words are ANDed.
        assert_eq!(top(&paths, "lib test"), vec!["lib/mail/main_test.go"]);
        // Regex mode.
        assert_eq!(top(&paths, r"\.md$"), vec!["docs/readme.md"]);
        assert_eq!(top(&paths, "").len(), 5);
        // Exclusions.
        let with = top(&paths, "main");
        let without = top(&paths, "main -test");
        assert!(with.contains(&"lib/mail/main_test.go".to_string()));
        assert!(!without.contains(&"lib/mail/main_test.go".to_string()));
        assert_eq!(without.len(), with.len() - 1);
        assert_eq!(
            top(&paths, "main -file:test -vendor -domain"),
            vec!["src/main.rs"]
        );
        assert_eq!(top(&paths, r"\.rs$ -file:^vendor/").len(), 2);
        assert_eq!(top(&paths, "-file:src").len(), 3);
        assert_eq!(top(&paths, "main -[x"), top(&paths, "main"));
        let m = fuzzy("src/main.rs", &["mrs"]).unwrap();
        assert_eq!(m.1, vec![4, 9, 10]);
    }

    #[test]
    fn dirs_lists_every_parent_once() {
        let paths: Vec<String> = [
            "README.md",
            "src/main.rs",
            "src/widget/draw.c",
            "src/widget/deep/x.c",
            "docs/a.md",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            dirs(&paths),
            vec!["docs", "src", "src/widget", "src/widget/deep"]
        );
        assert!(dirs(&["top.txt".to_string()]).is_empty());
    }

    #[test]
    fn loose_words_find_a_deep_folder() {
        let paths: Vec<String> = [
            "a/b/c/d/e/f/g/h/nvakbva/main.c",
            "a/b/c/d/e/f/g/h/nvakbva/sub/util.c",
            "a/b/c/d/e/f/g/h/other/x.c",
            "a/b/c/d/e/f/g/h/x.c",
            "a/h/bvx/y.c",
            "b/v/a/z.c",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let d = dirs(&paths);
        let hits: Vec<String> = find(&d, "a h bva", None, 10)
            .unwrap()
            .into_iter()
            .map(|m| m.path)
            .collect();
        assert_eq!(hits[0], "a/b/c/d/e/f/g/h/nvakbva");
        assert!(hits.contains(&"a/b/c/d/e/f/g/h/nvakbva/sub".to_string()));
        assert!(!hits.contains(&"a/b/c/d/e/f/g/h/other".to_string()));
        // The last segment counts most: `nvak` finds the folder itself first.
        assert_eq!(
            find(&d, "nvak", None, 10).unwrap()[0].path,
            "a/b/c/d/e/f/g/h/nvakbva"
        );
        // A regex and an exclusion work as they do for files.
        let r = find(&d, r"/sub$", None, 10).unwrap();
        assert_eq!(r.len(), 1);
        assert!(find(&d, "a h bva -sub", None, 10)
            .unwrap()
            .iter()
            .all(|m| !m.path.ends_with("/sub")));
    }
}
