//! Symbol extraction with tree-sitter.
//!
//! For each source file we record definitions (functions, methods, types,
//! macros, Bazel targets, ...) and call sites by identifier name. Calls also
//! carry the name of the function they appear in. Nothing here resolves
//! types: two functions with the same name are the same symbol to us.

mod queries;

use std::cell::RefCell;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCursor};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Kind {
    Function = 1,
    Method,
    Class,
    Struct,
    Enum,
    Union,
    Type,
    Interface,
    Trait,
    Module,
    Macro,
    Constant,
    Variable,
    Prototype,
    Target,
    Call,
}

impl Kind {
    pub const ALL: [Kind; 16] = [
        Kind::Function,
        Kind::Method,
        Kind::Class,
        Kind::Struct,
        Kind::Enum,
        Kind::Union,
        Kind::Type,
        Kind::Interface,
        Kind::Trait,
        Kind::Module,
        Kind::Macro,
        Kind::Constant,
        Kind::Variable,
        Kind::Prototype,
        Kind::Target,
        Kind::Call,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Function => "function",
            Kind::Method => "method",
            Kind::Class => "class",
            Kind::Struct => "struct",
            Kind::Enum => "enum",
            Kind::Union => "union",
            Kind::Type => "type",
            Kind::Interface => "interface",
            Kind::Trait => "trait",
            Kind::Module => "module",
            Kind::Macro => "macro",
            Kind::Constant => "constant",
            Kind::Variable => "variable",
            Kind::Prototype => "prototype",
            Kind::Target => "target",
            Kind::Call => "call",
        }
    }

    pub fn from_name(s: &str) -> Option<Kind> {
        Kind::ALL.iter().copied().find(|k| k.name() == s)
    }

    pub fn from_u8(v: u8) -> Option<Kind> {
        Kind::ALL.get((v as usize).wrapping_sub(1)).copied()
    }

    /// Kinds whose body can contain calls; used to name a call's caller.
    pub fn is_callable(self) -> bool {
        matches!(self, Kind::Function | Kind::Method)
    }

    pub fn is_def(self) -> bool {
        self != Kind::Call
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    C,
    Cpp,
    Rust,
    Go,
    Python,
    Starlark,
    Lua,
    JavaScript,
    TypeScript,
    Tsx,
}

const NLANG: usize = 10;

impl Lang {
    fn idx(self) -> usize {
        self as usize
    }

    fn language(self) -> Language {
        match self {
            Lang::C => tree_sitter_c::LANGUAGE.into(),
            Lang::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            Lang::Go => tree_sitter_go::LANGUAGE.into(),
            Lang::Python | Lang::Starlark => tree_sitter_python::LANGUAGE.into(),
            Lang::Lua => tree_sitter_lua::LANGUAGE.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        }
    }

    fn query_src(self) -> String {
        match self {
            Lang::C => queries::C.into(),
            Lang::Cpp => queries::CPP.into(),
            Lang::Rust => queries::RUST.into(),
            Lang::Go => queries::GO.into(),
            Lang::Python => queries::python(),
            Lang::Starlark => queries::starlark(),
            Lang::Lua => queries::LUA.into(),
            Lang::JavaScript => queries::javascript(),
            Lang::TypeScript | Lang::Tsx => queries::typescript(),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Lang::C => "c",
            Lang::Cpp => "cpp",
            Lang::Rust => "rust",
            Lang::Go => "go",
            Lang::Python => "python",
            Lang::Starlark => "starlark",
            Lang::Lua => "lua",
            Lang::JavaScript => "javascript",
            Lang::TypeScript => "typescript",
            Lang::Tsx => "tsx",
        }
    }

    /// Picks a language from a path (and for `.h`, a peek at the content).
    pub fn detect(path: &[u8], data: &[u8]) -> Option<Lang> {
        let base = match path.iter().rposition(|&b| b == b'/') {
            Some(i) => &path[i + 1..],
            None => path,
        };
        match base {
            b"BUILD" | b"BUILD.bazel" | b"WORKSPACE" | b"WORKSPACE.bazel" | b"MODULE.bazel" => {
                return Some(Lang::Starlark)
            }
            _ => {}
        }
        let ext = match base.iter().rposition(|&b| b == b'.') {
            Some(i) => &base[i + 1..],
            None => return None,
        };
        Some(match ext {
            b"c" => Lang::C,
            b"h" => {
                if looks_like_cpp(data) {
                    Lang::Cpp
                } else {
                    Lang::C
                }
            }
            b"cc" | b"cpp" | b"cxx" | b"c++" | b"hh" | b"hpp" | b"hxx" | b"h++" | b"ipp"
            | b"inl" | b"cu" | b"cuh" | b"tcc" => Lang::Cpp,
            b"rs" => Lang::Rust,
            b"go" => Lang::Go,
            b"py" | b"pyi" => Lang::Python,
            b"bzl" | b"star" | b"bazel" => Lang::Starlark,
            b"lua" => Lang::Lua,
            b"js" | b"mjs" | b"cjs" | b"jsx" => Lang::JavaScript,
            b"ts" | b"mts" | b"cts" => Lang::TypeScript,
            b"tsx" => Lang::Tsx,
            _ => return None,
        })
    }
}

fn looks_like_cpp(data: &[u8]) -> bool {
    let head = &data[..data.len().min(64 * 1024)];
    head.split(|&b| b == b'\n').any(|line| {
        let t = trim_start(line);
        t.starts_with(b"namespace ")
            || t.starts_with(b"using namespace ")
            || t.starts_with(b"template<")
            || t.starts_with(b"template <")
            || t.starts_with(b"public:")
            || t.starts_with(b"private:")
            || t.starts_with(b"protected:")
            || (t.starts_with(b"class ") && !t.ends_with(b";"))
            // <vector>, <string>: C++ standard headers have no extension.
            || (t.starts_with(b"#include <") && !t.contains(&b'.'))
            || memmem(t, b"std::")
    })
}

fn trim_start(b: &[u8]) -> &[u8] {
    let n = b.iter().take_while(|c| c.is_ascii_whitespace()).count();
    &b[n..]
}

fn memmem(h: &[u8], n: &[u8]) -> bool {
    h.windows(n.len()).any(|w| w == n)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sym {
    pub name: String,
    pub kind: Kind,
    /// 1-based line and 0-based byte column of the name.
    pub line: u32,
    pub col: u32,
    /// For calls: the function or method the call is in.
    pub container: Option<String>,
}

struct Compiled {
    query: Query,
    name_idx: u32,
    /// Capture index -> kind, for the outer captures.
    kinds: Vec<Option<Kind>>,
}

fn compiled(lang: Lang) -> &'static Compiled {
    static CACHE: [OnceLock<Compiled>; NLANG] = [const { OnceLock::new() }; NLANG];
    CACHE[lang.idx()].get_or_init(|| {
        let query = Query::new(&lang.language(), &lang.query_src())
            .unwrap_or_else(|e| panic!("bad {} query: {e}", lang.name()));
        let name_idx = query.capture_index_for_name("name").expect("@name capture");
        let kinds = query
            .capture_names()
            .iter()
            .map(|n| {
                if *n == "call" {
                    Some(Kind::Call)
                } else {
                    n.strip_prefix("def.")
                        .map(|k| Kind::from_name(k).unwrap_or_else(|| panic!("kind {k}")))
                }
            })
            .collect();
        Compiled {
            query,
            name_idx,
            kinds,
        }
    })
}

thread_local! {
    static PARSERS: RefCell<Vec<Option<Parser>>> = RefCell::new((0..NLANG).map(|_| None).collect());
}

/// Parsing stops after this long; whatever was found so far is kept.
pub const PARSE_BUDGET: Duration = Duration::from_millis(500);
const MAX_NAME: usize = 200;

/// Parses without querying; for profiling.
#[doc(hidden)]
pub fn parse_only(lang: Lang, data: &[u8]) -> bool {
    parse(lang, data).is_some()
}

fn parse(lang: Lang, data: &[u8]) -> Option<tree_sitter::Tree> {
    PARSERS.with(|p| {
        let mut p = p.borrow_mut();
        let parser = p[lang.idx()].get_or_insert_with(|| {
            let mut parser = Parser::new();
            parser
                .set_language(&lang.language())
                .expect("grammar version");
            parser
        });
        let start = Instant::now();
        let mut over = |_: &tree_sitter::ParseState| start.elapsed() > PARSE_BUDGET;
        let opts = tree_sitter::ParseOptions::new().progress_callback(&mut over);
        let len = data.len();
        let r = parser.parse_with_options(
            &mut |i, _| if i < len { &data[i..] } else { &[] as &[u8] },
            None,
            Some(opts),
        );
        parser.reset();
        r
    })
}

/// Extracts definitions and calls from one file.
pub fn extract(lang: Lang, data: &[u8]) -> Vec<Sym> {
    let c = compiled(lang);
    let Some(tree) = parse(lang, data) else {
        return Vec::new();
    };

    // (name_start, pattern, kind, outer byte range, name range, point)
    struct Raw {
        name_start: usize,
        pattern: usize,
        kind: Kind,
        outer: (usize, usize),
        name: (usize, usize),
        line: u32,
        col: u32,
    }
    let mut raws: Vec<Raw> = Vec::new();
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(1024);
    let mut it = cursor.matches(&c.query, tree.root_node(), data);
    while let Some(m) = it.next() {
        let mut name = None;
        let mut outer = None;
        for cap in m.captures {
            if cap.index == c.name_idx {
                name = Some(cap.node);
            } else if let Some(k) = c.kinds[cap.index as usize] {
                outer = Some((k, cap.node));
            }
        }
        let (Some(n), Some((kind, o))) = (name, outer) else {
            continue;
        };
        let r = n.byte_range();
        if r.is_empty() || r.len() > MAX_NAME {
            continue;
        }
        let p = n.start_position();
        raws.push(Raw {
            name_start: r.start,
            pattern: m.pattern_index,
            kind,
            outer: (o.start_byte(), o.end_byte()),
            name: (r.start, r.end),
            line: p.row as u32 + 1,
            col: p.column as u32,
        });
    }
    raws.sort_by_key(|r| (r.name_start, r.kind == Kind::Call, r.pattern));
    raws.dedup_by(|b, a| {
        a.name_start == b.name_start && (a.kind == Kind::Call) == (b.kind == Kind::Call)
    });

    // Name each call's enclosing function: sweep by start position with a
    // stack of open callable definitions.
    let mut events: Vec<(usize, usize, usize)> = Vec::new(); // (start, order, raw idx)
    for (i, r) in raws.iter().enumerate() {
        if r.kind.is_callable() || r.kind == Kind::Call {
            let is_call = r.kind == Kind::Call;
            events.push((
                if is_call { r.name_start } else { r.outer.0 },
                is_call as usize,
                i,
            ));
        }
    }
    events.sort_unstable();
    let mut container: Vec<Option<usize>> = vec![None; raws.len()];
    let mut stack: Vec<usize> = Vec::new();
    for &(start, _, i) in &events {
        while let Some(&top) = stack.last() {
            if raws[top].outer.1 <= start {
                stack.pop();
            } else {
                break;
            }
        }
        if raws[i].kind == Kind::Call {
            container[i] = stack.last().copied();
        } else {
            stack.push(i);
        }
    }

    let text = |r: (usize, usize)| String::from_utf8_lossy(&data[r.0..r.1]).into_owned();
    raws.iter()
        .enumerate()
        .filter(|(_, r)| r.kind != Kind::Call || is_ident(&data[r.name.0..r.name.1]))
        .map(|(i, r)| Sym {
            name: text(r.name),
            kind: r.kind,
            line: r.line,
            col: r.col,
            container: container[i].map(|ci| text(raws[ci].name)),
        })
        .collect()
}

fn is_ident(b: &[u8]) -> bool {
    b.iter()
        .all(|&c| c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80)
}

/// Convenience: detect the language and extract, or return nothing.
pub fn extract_path(path: &[u8], data: &[u8]) -> Vec<Sym> {
    match Lang::detect(path, data) {
        Some(l) => extract(l, data),
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests;
