//! Query syntax, after Google Code Search
//! (developers.google.com/code-search/reference):
//!
//! - terms are RE2-style regexes, case insensitive by default;
//! - several terms are ANDed (per file), `OR` and `(...)` group, `-` excludes;
//! - `"..."` is a literal, `\` escapes the next character;
//! - a plain term matches file paths as well as file contents;
//! - filters: `content:` (contents only), `file:`/`filepath:`/`path:`/`f:`,
//!   `lang:`/`language:`, `case:yes|no|auto` (inherited by child groups),
//!   `comment:`, `usage:` (outside comments and strings), `pcre:yes`
//!   (the regex may span lines), and symbol filters `sym:`/`symbol:`,
//!   `function:`/`func:`, `class:`, `calls:` and `refs:`.

use anyhow::{bail, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Case {
    Smart,
    Sensitive,
    Insensitive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TermKind {
    /// Matches the file path or the contents.
    Text,
    /// `content:` — contents only.
    Content,
    /// `file:`/`lang:` — the path only.
    Path,
    /// `comment:` — inside comments.
    Comment,
    /// `usage:` — outside comments and string literals.
    Usage,
}

impl TermKind {
    pub fn reads_content(self) -> bool {
        self != TermKind::Path
    }
}

#[derive(Clone, Debug)]
pub struct Term {
    pub kind: TermKind,
    /// Regex source (already escaped for quoted terms).
    pub pattern: String,
    /// `None` uses the search's default case.
    pub case: Option<Case>,
    /// From a quoted term; `-F` doesn't need to escape it again.
    pub literal: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    True,
    Term(usize),
    Not(Box<Expr>),
    And(Vec<Expr>),
    Or(Vec<Expr>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymMode {
    /// `sym:` — definitions.
    Defs,
    /// `calls:` — call sites.
    Calls,
}

#[derive(Clone, Debug)]
pub struct Parsed {
    pub terms: Vec<Term>,
    pub expr: Expr,
    /// `pcre:yes`: content regexes may match across lines.
    pub pcre: bool,
    /// Top-level `case:` (for symbol names).
    pub case: Option<Case>,
    /// Symbol search instead of text search.
    pub sym: Option<SymMode>,
    /// The name (or name regex) for `sym:`/`calls:`.
    pub name: String,
    /// `kind:` filters for symbol search.
    pub kinds: Vec<String>,
}

impl Default for Parsed {
    fn default() -> Self {
        Parsed {
            terms: Vec::new(),
            expr: Expr::True,
            pcre: false,
            case: None,
            sym: None,
            name: String::new(),
            kinds: Vec::new(),
        }
    }
}

impl Parsed {
    /// A whole query taken as one regex, no syntax.
    pub fn raw(query: &str) -> Parsed {
        Parsed {
            terms: vec![Term { kind: TermKind::Text, pattern: query.to_string(), case: None, literal: false }],
            expr: Expr::Term(0),
            ..Default::default()
        }
    }

    /// Terms that are not under a `-`, i.e. ones whose matches are shown.
    pub fn positive_terms(&self) -> Vec<usize> {
        fn walk(e: &Expr, neg: bool, out: &mut Vec<usize>) {
            match e {
                Expr::True => {}
                Expr::Term(i) => {
                    if !neg {
                        out.push(*i)
                    }
                }
                Expr::Not(x) => walk(x, !neg, out),
                Expr::And(xs) | Expr::Or(xs) => xs.iter().for_each(|x| walk(x, neg, out)),
            }
        }
        let mut v = Vec::new();
        walk(&self.expr, false, &mut v);
        v.sort_unstable();
        v.dedup();
        v
    }
}

/// `Foo::bar`, `Foo.bar` and `Foo#bar` name `bar`; symbols are stored by
/// their last component.
fn last_component(v: &str) -> &str {
    let cut = [v.rfind("::").map(|i| i + 2), v.rfind('.').map(|i| i + 1), v.rfind('#').map(|i| i + 1)]
        .into_iter()
        .flatten()
        .max();
    match cut {
        // Keep regex-looking values (`a.b`, `foo.*`) intact.
        Some(i) if i < v.len() && v[i..].chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$') && !v.contains('*') => {
            &v[i..]
        }
        _ => v,
    }
}

/// File extension (or exact file name) regexes per language name. Names and
/// aliases follow Google Code Search's `lang:` table, plus a few more.
pub fn lang_regex(lang: &str) -> Option<String> {
    let exts: &[&str] = match lang.to_ascii_lowercase().as_str() {
        "c" => &["c", "h"],
        "cpp" | "c++" | "cxx" | "cc" => &["cc", "cpp", "cxx", "c\\+\\+", "hh", "hpp", "hxx", "h", "inl", "ipp"],
        "cuda" | "cu" => &["cu", "cuh"],
        "go" | "golang" => &["go"],
        "rust" | "rs" => &["rs"],
        "python" | "py" => &["py", "pyi"],
        "js" | "javascript" => &["js", "jsx", "mjs", "cjs"],
        "ts" | "typescript" => &["ts", "tsx", "mts", "cts"],
        "java" => &["java"],
        "kotlin" | "kt" | "kts" => &["kt", "kts"],
        "lua" => &["lua"],
        "sh" | "shell" | "bash" => &["sh", "bash", "zsh"],
        "md" | "markdown" => &["md", "markdown"],
        "json" => &["json"],
        "yaml" | "yml" => &["yaml", "yml"],
        "toml" => &["toml"],
        "proto" | "protobuf" => &["proto"],
        "bazel" | "bzl" | "starlark" => {
            return Some(r"(^|/)(BUILD|BUILD\.bazel|WORKSPACE|WORKSPACE\.bazel|MODULE\.bazel)$|\.bzl$".into())
        }
        "cmake" => return Some(r"(^|/)CMakeLists\.txt$|\.cmake$".into()),
        "make" | "makefile" => return Some(r"(^|/)(GNUmakefile|Makefile|makefile)$|\.mk$".into()),
        "swift" => &["swift"],
        "ruby" | "rb" => &["rb"],
        "c#" | "cs" | "csharp" => &["cs"],
        "scala" => &["scala"],
        "sql" => &["sql"],
        "html" | "angular" => &["html", "htm"],
        "shtml" => &["shtml"],
        "css" => &["css", "scss", "sass", "less"],
        "vim" => &["vim"],
        "objectivec" | "objc" | "objective-c" => &["m", "h"],
        "objectivec++" | "objc++" => &["mm", "h"],
        "applescript" => &["applescript", "scpt"],
        "csv" => &["csv"],
        "dart" => &["dart"],
        "django" => &["djhtml", "html"],
        "jsp" => &["jsp"],
        "perl" | "pl" => &["pl", "pm", "t"],
        "php" => &["php"],
        "powershell" | "ps1" => &["ps1", "psm1", "psd1"],
        "tcl" => &["tcl"],
        "xml" => &["xml"],
        "xslt" | "xsl" => &["xsl", "xslt"],
        _ => return None,
    };
    Some(format!(r"\.({})$", exts.join("|")))
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    LParen,
    RParen,
    Or,
    And,
    Neg,
    /// `key` is a recognised filter name; `quoted` means the value was in
    /// double quotes.
    Atom { key: Option<String>, value: String, quoted: bool },
}

const KEYS: &[&str] = &[
    "file", "filepath", "path", "f", "lang", "language", "l", "case", "content", "comment", "usage", "pcre", "sym",
    "s", "symbol", "function", "func", "class", "calls", "callers", "c", "refs", "ref", "r", "kind", "k",
];

/// Reads a double-quoted literal starting after the opening quote. `\"` and
/// `\\` are escapes; any other backslash is kept.
fn quoted(chars: &[char], i: &mut usize) -> Result<String> {
    let mut s = String::new();
    while *i < chars.len() {
        let c = chars[*i];
        *i += 1;
        match c {
            '"' => return Ok(s),
            '\\' if *i < chars.len() && (chars[*i] == '"' || chars[*i] == '\\') => {
                s.push(chars[*i]);
                *i += 1;
            }
            _ => s.push(c),
        }
    }
    bail!("unclosed quote")
}

fn tokenize(q: &str) -> Result<Vec<Tok>> {
    let chars: Vec<char> = q.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '(' && chars.get(i + 1) != Some(&'?') {
            out.push(Tok::LParen);
            i += 1;
            continue;
        }
        if c == ')' {
            out.push(Tok::RParen);
            i += 1;
            continue;
        }
        if c == '-' && chars.get(i + 1).is_some_and(|n| !n.is_whitespace()) {
            out.push(Tok::Neg);
            i += 1;
            continue;
        }
        if c == '"' {
            i += 1;
            let s = quoted(&chars, &mut i)?;
            out.push(Tok::Atom { key: None, value: s, quoted: true });
            continue;
        }
        // A word: up to whitespace, or a `)` that isn't closing a `(` inside
        // the word (so `foo()` stays a regex).
        let mut word = String::new();
        let mut depth = 0i32;
        let mut key: Option<String> = None;
        while i < chars.len() {
            let c = chars[i];
            if c.is_whitespace() || (c == ')' && depth == 0) {
                break;
            }
            if c == '\\' && i + 1 < chars.len() {
                word.push(c);
                word.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == ':' && key.is_none() && KEYS.contains(&word.as_str()) {
                key = Some(std::mem::take(&mut word));
                i += 1;
                if chars.get(i) == Some(&'"') {
                    i += 1;
                    let v = quoted(&chars, &mut i)?;
                    out.push(Tok::Atom { key: key.take(), value: v, quoted: true });
                    word.clear();
                    break;
                }
                continue;
            }
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            word.push(c);
            i += 1;
        }
        match key {
            Some(k) if !word.is_empty() => out.push(Tok::Atom { key: Some(k), value: word, quoted: false }),
            // `file:` with nothing after it is just text.
            Some(k) => out.push(Tok::Atom { key: None, value: format!("{}:", k), quoted: false }),
            None if word == "OR" => out.push(Tok::Or),
            None if word == "AND" => out.push(Tok::And),
            None if word.is_empty() => {}
            None => out.push(Tok::Atom { key: None, value: word, quoted: false }),
        }
    }
    Ok(out)
}

/// Parsed but not yet lowered: case is resolved after a group is read,
/// because `case:` anywhere in a group applies to the whole group.
enum Raw {
    True,
    Leaf { kind: TermKind, pattern: String, literal: bool, force_case: Option<Case> },
    Not(Box<Raw>),
    And(Vec<Raw>),
    Or(Vec<Raw>),
    Group { case: Option<Case>, body: Box<Raw> },
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    /// One slot per open group for its `case:`.
    cases: Vec<Option<Case>>,
    depth: usize,
    neg: usize,
    in_or: usize,
    pcre: bool,
    sym: Option<SymMode>,
    name: String,
    kinds: Vec<String>,
    sym_kinds: Vec<String>,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn or(&mut self) -> Result<Raw> {
        let mut alts = vec![self.and()?];
        while self.peek() == Some(&Tok::Or) {
            self.pos += 1;
            self.in_or += 1;
            let a = self.and();
            self.in_or -= 1;
            alts.push(a?);
        }
        if alts.len() > 1 {
            // Symbol filters inside OR are rejected in `atom`, but the first
            // branch was parsed before we knew; check it here.
            if self.sym.is_some() && self.depth == 0 && self.in_or == 0 {
                // fine only if the sym atom is outside this OR; we can't tell
                // cheaply, so be strict.
                bail!("sym:, function:, class: and calls: can't be used with OR");
            }
            Ok(Raw::Or(alts))
        } else {
            Ok(alts.pop().unwrap())
        }
    }

    fn and(&mut self) -> Result<Raw> {
        let mut xs = Vec::new();
        loop {
            match self.peek() {
                None | Some(Tok::RParen) | Some(Tok::Or) => break,
                Some(Tok::And) => {
                    self.pos += 1;
                    continue;
                }
                _ => xs.push(self.unary()?),
            }
        }
        if xs.is_empty() {
            bail!("empty query or group");
        }
        Ok(if xs.len() == 1 { xs.pop().unwrap() } else { Raw::And(xs) })
    }

    fn unary(&mut self) -> Result<Raw> {
        match self.peek().cloned() {
            Some(Tok::Neg) => {
                self.pos += 1;
                if matches!(self.peek(), None | Some(Tok::RParen) | Some(Tok::Or) | Some(Tok::And)) {
                    return Ok(Raw::Leaf { kind: TermKind::Text, pattern: "-".into(), literal: true, force_case: None });
                }
                self.neg += 1;
                let x = self.unary();
                self.neg -= 1;
                Ok(Raw::Not(Box::new(x?)))
            }
            Some(Tok::LParen) => {
                self.pos += 1;
                self.depth += 1;
                self.cases.push(None);
                let body = self.or();
                let case = self.cases.pop().unwrap();
                self.depth -= 1;
                let body = body?;
                if self.peek() != Some(&Tok::RParen) {
                    bail!("missing `)`");
                }
                self.pos += 1;
                Ok(Raw::Group { case, body: Box::new(body) })
            }
            Some(Tok::RParen) => bail!("unmatched `)`"),
            Some(Tok::Atom { key, value, quoted }) => {
                self.pos += 1;
                self.atom(key.as_deref(), value, quoted)
            }
            Some(Tok::Or) | Some(Tok::And) => {
                // A leading or doubled operator is literal text.
                self.pos += 1;
                let w = if self.toks[self.pos - 1] == Tok::Or { "OR" } else { "AND" };
                Ok(Raw::Leaf { kind: TermKind::Text, pattern: w.into(), literal: true, force_case: None })
            }
            None => bail!("unexpected end of query"),
        }
    }

    fn leaf(kind: TermKind, value: String, quoted: bool) -> Raw {
        let pattern = if quoted { regex_syntax::escape(&value) } else { value };
        Raw::Leaf { kind, pattern, literal: quoted, force_case: None }
    }

    fn atom(&mut self, key: Option<&str>, value: String, quoted: bool) -> Result<Raw> {
        let Some(key) = key else { return Ok(Self::leaf(TermKind::Text, value, quoted)) };
        let only_top = |p: &Parser, what: &str| -> Result<()> {
            if p.neg > 0 || p.in_or > 0 || p.depth > 0 {
                bail!("{} can't be negated, grouped or used with OR", what);
            }
            Ok(())
        };
        Ok(match key {
            "file" | "filepath" | "path" | "f" => Self::leaf(TermKind::Path, value, quoted),
            "lang" | "language" | "l" => {
                let Some(re) = lang_regex(&value) else { bail!("unknown language `{}`", value) };
                Raw::Leaf { kind: TermKind::Path, pattern: re, literal: false, force_case: Some(Case::Sensitive) }
            }
            "content" => Self::leaf(TermKind::Content, value, quoted),
            "comment" => Self::leaf(TermKind::Comment, value, quoted),
            "usage" => Self::leaf(TermKind::Usage, value, quoted),
            "case" => {
                if self.neg > 0 {
                    bail!("case: can't be negated");
                }
                let c = match value.as_str() {
                    "yes" | "y" | "sensitive" => Case::Sensitive,
                    "no" | "n" | "insensitive" => Case::Insensitive,
                    "auto" | "smart" => Case::Smart,
                    _ => bail!("case: takes yes, no or auto"),
                };
                *self.cases.last_mut().unwrap() = Some(c);
                Raw::True
            }
            "pcre" => {
                only_top(self, "pcre:")?;
                match value.as_str() {
                    "yes" | "y" => self.pcre = true,
                    "no" | "n" => self.pcre = false,
                    _ => bail!("pcre: takes yes or no"),
                }
                Raw::True
            }
            "kind" | "k" => {
                only_top(self, "kind:")?;
                self.kinds.extend(value.split(',').map(str::to_string));
                Raw::True
            }
            "sym" | "s" | "symbol" | "function" | "func" | "class" | "calls" | "callers" | "c" => {
                only_top(self, "sym:, function:, class: and calls:")?;
                if self.sym.is_some() {
                    bail!("use one symbol filter (sym:, function:, class: or calls:) per query");
                }
                self.sym = Some(if matches!(key, "calls" | "callers" | "c") { SymMode::Calls } else { SymMode::Defs });
                match key {
                    "function" | "func" => self.sym_kinds.push("function".into()),
                    "class" => self.sym_kinds.push("class".into()),
                    _ => {}
                }
                self.name = if quoted { regex_syntax::escape(&value) } else { last_component(&value).to_string() };
                Raw::True
            }
            "refs" | "ref" | "r" => {
                let name = last_component(&value);
                Raw::Leaf {
                    kind: TermKind::Content,
                    pattern: format!(r"\b{}\b", regex_syntax::escape(name)),
                    literal: true,
                    force_case: Some(Case::Sensitive),
                }
            }
            _ => Self::leaf(TermKind::Text, format!("{}:{}", key, value), quoted),
        })
    }
}

fn lower(raw: Raw, case: Option<Case>, terms: &mut Vec<Term>) -> Expr {
    match raw {
        Raw::True => Expr::True,
        Raw::Leaf { kind, pattern, literal, force_case } => {
            // An explicit case: wins over refs:'s default; lang: is always
            // exact (extensions).
            let c = if kind == TermKind::Path && force_case.is_some() { force_case } else { case.or(force_case) };
            terms.push(Term { kind, pattern, case: c, literal });
            Expr::Term(terms.len() - 1)
        }
        Raw::Not(x) => match lower(*x, case, terms) {
            Expr::True => Expr::True,
            e => Expr::Not(Box::new(e)),
        },
        Raw::Group { case: c, body } => lower(*body, c.or(case), terms),
        Raw::And(xs) => {
            let mut v: Vec<Expr> = xs.into_iter().map(|x| lower(x, case, terms)).filter(|e| *e != Expr::True).collect();
            match v.len() {
                0 => Expr::True,
                1 => v.pop().unwrap(),
                _ => Expr::And(v),
            }
        }
        Raw::Or(xs) => {
            let v: Vec<Expr> = xs.into_iter().map(|x| lower(x, case, terms)).collect();
            if v.contains(&Expr::True) {
                Expr::True
            } else {
                Expr::Or(v)
            }
        }
    }
}

pub fn parse(query: &str) -> Result<Parsed> {
    let toks = tokenize(query)?;
    if toks.is_empty() {
        bail!("empty query");
    }
    let mut p = Parser {
        toks,
        pos: 0,
        cases: vec![None],
        depth: 0,
        neg: 0,
        in_or: 0,
        pcre: false,
        sym: None,
        name: String::new(),
        kinds: Vec::new(),
        sym_kinds: Vec::new(),
    };
    let raw = p.or()?;
    if p.pos < p.toks.len() {
        bail!("unmatched `)`");
    }
    let top_case = p.cases.pop().unwrap();
    let mut terms = Vec::new();
    let expr = lower(raw, top_case, &mut terms);
    let mut kinds = p.kinds;
    if kinds.is_empty() {
        kinds = p.sym_kinds;
    }
    if !kinds.is_empty() && p.sym.is_none() {
        bail!("kind: only works with sym: or calls:");
    }
    if p.sym.is_some() && p.name.is_empty() {
        bail!("sym: and calls: need a name");
    }
    Ok(Parsed { terms, expr, pcre: p.pcre, case: top_case, sym: p.sym, name: p.name, kinds })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pats(p: &Parsed) -> Vec<(TermKind, String)> {
        p.terms.iter().map(|t| (t.kind, t.pattern.clone())).collect()
    }

    #[test]
    fn implicit_and_filters_and_case() {
        let p = parse("foo bar file:src/ -file:_test lang:go case:yes").unwrap();
        assert_eq!(
            pats(&p),
            vec![
                (TermKind::Text, "foo".into()),
                (TermKind::Text, "bar".into()),
                (TermKind::Path, "src/".into()),
                (TermKind::Path, "_test".into()),
                (TermKind::Path, r"\.(go)$".into()),
            ]
        );
        assert_eq!(
            p.expr,
            Expr::And(vec![
                Expr::Term(0),
                Expr::Term(1),
                Expr::Term(2),
                Expr::Not(Box::new(Expr::Term(3))),
                Expr::Term(4)
            ])
        );
        // case: anywhere in the group applies to all of it.
        assert!(p.terms[..4].iter().all(|t| t.case == Some(Case::Sensitive)));
        assert_eq!(p.positive_terms(), vec![0, 1, 2, 4]);
    }

    #[test]
    fn or_groups_quotes_escapes() {
        let p = parse("(hello OR world) AND lang:java").unwrap();
        assert_eq!(
            p.expr,
            Expr::And(vec![Expr::Or(vec![Expr::Term(0), Expr::Term(1)]), Expr::Term(2)])
        );
        // OR binds looser than AND.
        let p = parse("a b OR c").unwrap();
        assert_eq!(p.expr, Expr::Or(vec![Expr::And(vec![Expr::Term(0), Expr::Term(1)]), Expr::Term(2)]));
        let p = parse(r#""foo()" "a \"b\" c" main\.java f:main\.java"#).unwrap();
        assert_eq!(p.terms[0].pattern, r"foo\(\)");
        assert_eq!(p.terms[1].pattern, regex_syntax::escape(r#"a "b" c"#));
        assert_eq!(p.terms[2].pattern, r"main\.java");
        assert_eq!((p.terms[3].kind, p.terms[3].pattern.as_str()), (TermKind::Path, r"main\.java"));
        // Parens inside a word stay regex; `(?i)` isn't a group.
        let p = parse("foo(bar|baz) (?i)x").unwrap();
        assert_eq!(p.terms[0].pattern, "foo(bar|baz)");
        assert_eq!(p.terms[1].pattern, "(?i)x");
        // Escaped space keeps one term.
        assert_eq!(parse(r"jobs:\ build").unwrap().terms.len(), 1);
        // Nested groups and case inheritance.
        let p = parse("((Strings OR StringBuilder) AND lang:java case:yes) OR (std AND lang:cpp)").unwrap();
        assert_eq!(p.terms[0].case, Some(Case::Sensitive));
        assert_eq!(p.terms[3].case, None);
        assert!(parse("(a").is_err());
        assert!(parse("a)").is_err());
        assert!(parse("\"a").is_err());
    }

    #[test]
    fn filter_aliases() {
        for q in ["file:x", "filepath:x", "path:x", "f:x"] {
            assert_eq!(parse(q).unwrap().terms[0].kind, TermKind::Path);
        }
        for q in ["lang:golang", "language:go", "lang:c++", "lang:objc", "lang:sh", "lang:kts"] {
            assert_eq!(parse(q).unwrap().terms[0].kind, TermKind::Path, "{q}");
        }
        assert!(parse("lang:cobol").is_err());
        assert_eq!(parse("content:main").unwrap().terms[0].kind, TermKind::Content);
        assert_eq!(parse("comment:bug").unwrap().terms[0].kind, TermKind::Comment);
        assert_eq!(parse("usage:map").unwrap().terms[0].kind, TermKind::Usage);
        assert!(parse("pcre:yes {\\s+x").unwrap().pcre);
        assert_eq!(parse("case:y X").unwrap().terms[0].case, Some(Case::Sensitive));
        // Unknown keys are text.
        assert_eq!(parse("std::vector").unwrap().terms[0].pattern, "std::vector");
        assert_eq!(parse("http://x").unwrap().terms[0].kind, TermKind::Text);
    }

    #[test]
    fn symbol_queries() {
        let p = parse("sym:Widget::Draw lang:cpp kind:method").unwrap();
        assert_eq!(p.sym, Some(SymMode::Defs));
        assert_eq!(p.name, "Draw");
        assert_eq!(p.kinds, vec!["method".to_string()]);
        let p = parse("calls:kmalloc file:mm/ GFP").unwrap();
        assert_eq!(p.sym, Some(SymMode::Calls));
        assert_eq!(p.name, "kmalloc");
        assert_eq!(p.terms[1].pattern, "GFP");
        assert_eq!(parse("sym:foo.*bar").unwrap().name, "foo.*bar");
        let p = parse("function:print").unwrap();
        assert_eq!((p.sym, p.kinds.clone()), (Some(SymMode::Defs), vec!["function".to_string()]));
        assert_eq!(parse("class:Main").unwrap().kinds, vec!["class".to_string()]);
        assert_eq!(parse("symbol:immutable").unwrap().kinds, Vec::<String>::new());
        let p = parse("refs:self.run").unwrap();
        assert_eq!(p.terms[0].pattern, r"\brun\b");
        assert_eq!(p.terms[0].case, Some(Case::Sensitive));
        assert!(parse("kind:function foo").is_err());
        assert!(parse("sym:a OR b").is_err());
        assert!(parse("-sym:a").is_err());
    }
}
