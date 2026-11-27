//! Comment and string-literal ranges for `comment:` and `usage:`, from a
//! small lexer per language family. It doesn't parse, so odd cases (nested
//! raw strings, heredocs) can be off; unknown file types have no regions.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Region {
    Comment,
    Str,
}

#[derive(Default, Debug)]
pub struct Regions {
    /// Sorted, non-overlapping byte ranges.
    pub spans: Vec<(usize, usize, Region)>,
    pub known: bool,
}

impl Regions {
    /// The region covering byte `at`, if any.
    pub fn at(&self, at: usize) -> Option<Region> {
        let i = self.spans.partition_point(|s| s.1 <= at);
        self.spans.get(i).filter(|s| s.0 <= at).map(|s| s.2)
    }

    /// Whether `[s, e)` lies inside one comment.
    pub fn in_comment(&self, s: usize, e: usize) -> bool {
        let i = self.spans.partition_point(|x| x.1 <= s);
        self.spans.get(i).is_some_and(|x| x.0 <= s && e <= x.1 && x.2 == Region::Comment)
    }

    /// Whether `[s, e)` touches no comment or string.
    pub fn in_code(&self, s: usize, e: usize) -> bool {
        let i = self.spans.partition_point(|x| x.1 <= s);
        self.spans.get(i).is_none_or(|x| x.0 >= e.max(s + 1))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    /// `//`, `/* */`, "..", '..' (and `..` where `backtick`).
    CLike { backtick: bool, hash: bool, rust: bool },
    /// `#` comments, "..", '..', Python triple quotes.
    Hash { triple: bool },
    /// `--`, `--[[ ]]`, [[ ]].
    Lua,
    /// `--`, `/* */`, '..'.
    Sql,
    /// `<!-- -->`.
    Markup,
}

fn family(path: &str) -> Option<Family> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = name.rsplit_once('.').map(|x| x.1.to_ascii_lowercase()).unwrap_or_default();
    let c = |backtick, hash| Some(Family::CLike { backtick, hash, rust: false });
    match ext.as_str() {
        "c" | "h" | "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" | "inl" | "ipp" | "cu" | "cuh" | "m" | "mm"
        | "java" | "kt" | "kts" | "cs" | "scala" | "swift" | "dart" | "proto" | "groovy" | "gradle" | "css"
        | "scss" | "less" | "json" | "jsonc" | "zig" | "v" | "sv" => c(false, false),
        "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts" | "go" => c(true, false),
        "php" => c(false, true),
        "rs" => Some(Family::CLike { backtick: false, hash: false, rust: true }),
        "py" | "pyi" | "bzl" | "star" => Some(Family::Hash { triple: true }),
        "sh" | "bash" | "zsh" | "rb" | "pl" | "pm" | "yaml" | "yml" | "toml" | "cmake" | "mk" | "r" | "nix"
        | "conf" | "cfg" | "ini" | "tcl" | "ps1" | "dockerfile" => Some(Family::Hash { triple: false }),
        "lua" => Some(Family::Lua),
        "sql" => Some(Family::Sql),
        "html" | "htm" | "xml" | "xsl" | "xslt" | "svg" | "vue" | "md" | "markdown" => Some(Family::Markup),
        _ => match name {
            "BUILD" | "BUILD.bazel" | "WORKSPACE" | "WORKSPACE.bazel" | "MODULE.bazel" => {
                Some(Family::Hash { triple: true })
            }
            "Makefile" | "makefile" | "GNUmakefile" | "Dockerfile" | "CMakeLists.txt" => {
                Some(Family::Hash { triple: false })
            }
            _ => None,
        },
    }
}

fn find(d: &[u8], from: usize, pat: &[u8]) -> usize {
    if from >= d.len() {
        return d.len();
    }
    d[from..].windows(pat.len()).position(|w| w == pat).map(|p| from + p + pat.len()).unwrap_or(d.len())
}

fn eol(d: &[u8], from: usize) -> usize {
    d[from.min(d.len())..].iter().position(|&b| b == b'\n').map(|p| from + p).unwrap_or(d.len())
}

/// End of a quoted string starting at `i` (the quote). Stops at a newline
/// unless `multiline`.
fn string_end(d: &[u8], i: usize, q: u8, escapes: bool, multiline: bool) -> usize {
    let mut j = i + 1;
    while j < d.len() {
        match d[j] {
            b'\\' if escapes => j += 2,
            b'\n' if !multiline => return j,
            b if b == q => return j + 1,
            _ => j += 1,
        }
    }
    d.len()
}

pub fn regions(path: &str, d: &[u8]) -> Regions {
    let Some(f) = family(path) else { return Regions::default() };
    let mut spans = Vec::new();
    let mut i = 0;
    let push = |spans: &mut Vec<(usize, usize, Region)>, s, e, r| {
        if e > s {
            spans.push((s, e, r));
        }
    };
    while i < d.len() {
        let b = d[i];
        let next = d.get(i + 1).copied();
        let (end, r) = match f {
            Family::CLike { backtick, hash, rust } => match b {
                b'/' if next == Some(b'/') => (eol(d, i), Region::Comment),
                b'/' if next == Some(b'*') => (find(d, i + 2, b"*/"), Region::Comment),
                b'#' if hash => (eol(d, i), Region::Comment),
                b'"' => (string_end(d, i, b'"', true, rust), Region::Str),
                b'`' if backtick => (string_end(d, i, b'`', false, true), Region::Str),
                b'r' if rust && matches!(next, Some(b'"') | Some(b'#'))
                    && (i == 0 || !(d[i - 1].is_ascii_alphanumeric() || d[i - 1] == b'_')) =>
                {
                    let mut j = i + 1;
                    while d.get(j) == Some(&b'#') {
                        j += 1;
                    }
                    if d.get(j) != Some(&b'"') {
                        i += 1;
                        continue;
                    }
                    let mut close = vec![b'"'];
                    close.extend(std::iter::repeat_n(b'#', j - i - 1));
                    (find(d, j + 1, &close), Region::Str)
                }
                b'\'' if rust => {
                    // A char literal is short; otherwise it's a lifetime.
                    let e = string_end(d, i, b'\'', true, false);
                    let closed = e > i + 2 && d.get(e - 1) == Some(&b'\'');
                    let inner = if closed { &d[i + 1..e - 1] } else { &d[i..i] };
                    let char_lit = (inner.starts_with(b"\\") && inner.len() <= 10)
                        || (inner.len() <= 4 && std::str::from_utf8(inner).is_ok_and(|s| s.chars().count() == 1));
                    if closed && char_lit {
                        (e, Region::Str)
                    } else {
                        i += 1;
                        continue;
                    }
                }
                b'\'' => (string_end(d, i, b'\'', true, false), Region::Str),
                _ => {
                    i += 1;
                    continue;
                }
            },
            Family::Hash { triple } => match b {
                b'#' => (eol(d, i), Region::Comment),
                b'"' | b'\'' if triple && d[i..].starts_with(&[b, b, b]) => (find(d, i + 3, &[b, b, b]), Region::Str),
                b'"' | b'\'' => (string_end(d, i, b, true, false), Region::Str),
                _ => {
                    i += 1;
                    continue;
                }
            },
            Family::Lua => match b {
                b'-' if next == Some(b'-') => {
                    if d[i + 2..].starts_with(b"[[") {
                        (find(d, i + 4, b"]]"), Region::Comment)
                    } else {
                        (eol(d, i), Region::Comment)
                    }
                }
                b'[' if next == Some(b'[') => (find(d, i + 2, b"]]"), Region::Str),
                b'"' | b'\'' => (string_end(d, i, b, true, false), Region::Str),
                _ => {
                    i += 1;
                    continue;
                }
            },
            Family::Sql => match b {
                b'-' if next == Some(b'-') => (eol(d, i), Region::Comment),
                b'/' if next == Some(b'*') => (find(d, i + 2, b"*/"), Region::Comment),
                b'\'' | b'"' => (string_end(d, i, b, false, true), Region::Str),
                _ => {
                    i += 1;
                    continue;
                }
            },
            Family::Markup => {
                if d[i..].starts_with(b"<!--") {
                    (find(d, i + 4, b"-->"), Region::Comment)
                } else {
                    i += 1;
                    continue;
                }
            }
        };
        push(&mut spans, i, end, r);
        i = end.max(i + 1);
    }
    Regions { spans, known: true }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text<'a>(d: &'a str, r: &Regions, k: Region) -> Vec<&'a str> {
        r.spans.iter().filter(|s| s.2 == k).map(|s| &d[s.0..s.1]).collect()
    }

    #[test]
    fn c_like() {
        let d = "int main(void) { // The main thing here\n  s = \"The main class.\"; /* a\nb */ c = 'x';\n}";
        let r = regions("a.c", d.as_bytes());
        assert_eq!(text(d, &r, Region::Comment), vec!["// The main thing here", "/* a\nb */"]);
        assert_eq!(text(d, &r, Region::Str), vec!["\"The main class.\"", "'x'"]);
        assert!(r.in_code(4, 8));
        assert!(!r.in_code(d.find("main thing").unwrap(), d.find("main thing").unwrap() + 4));
        assert!(r.in_comment(d.find("main thing").unwrap(), d.find("main thing").unwrap() + 4));
    }

    #[test]
    fn rust_python_lua() {
        let d = "fn f<'a>(x: &'a str) { let s = r#\"a \"q\" b\"#; let c = '\\n'; } // end";
        let r = regions("x.rs", d.as_bytes());
        assert_eq!(text(d, &r, Region::Str), vec!["r#\"a \"q\" b\"#", "'\\n'"]);
        assert_eq!(text(d, &r, Region::Comment), vec!["// end"]);
        let d = "x = '#no'  # yes\ns = \"\"\"doc\n# not a comment\"\"\"\n";
        let r = regions("a.py", d.as_bytes());
        assert_eq!(text(d, &r, Region::Comment), vec!["# yes"]);
        assert_eq!(text(d, &r, Region::Str).len(), 2);
        let d = "local x = 1 -- c\n--[[ long\n]] y = [[s]]";
        let r = regions("a.lua", d.as_bytes());
        assert_eq!(text(d, &r, Region::Comment), vec!["-- c", "--[[ long\n]]"]);
        assert_eq!(text(d, &r, Region::Str), vec!["[[s]]"]);
        assert!(!regions("a.unknownext", b"// x").known);
    }
}
