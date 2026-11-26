//! Query syntax: a regex plus optional `file:`, `-file:`, `lang:`,
//! `-lang:` and `case:` filters, zoekt style.

use anyhow::{bail, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Case {
    Smart,
    Sensitive,
    Insensitive,
}

#[derive(Clone, Debug, Default)]
pub struct Parsed {
    pub pattern: String,
    pub file: Vec<String>,
    pub not_file: Vec<String>,
    pub case: Option<Case>,
}

/// File extension (or exact file name) regexes per language name.
pub fn lang_regex(lang: &str) -> Option<String> {
    let exts: &[&str] = match lang.to_ascii_lowercase().as_str() {
        "c" => &["c", "h"],
        "cpp" | "c++" | "cxx" | "cc" => &["cc", "cpp", "cxx", "c\\+\\+", "hh", "hpp", "hxx", "h", "inl", "ipp"],
        "cuda" | "cu" => &["cu", "cuh"],
        "go" => &["go"],
        "rust" | "rs" => &["rs"],
        "python" | "py" => &["py", "pyi"],
        "js" | "javascript" => &["js", "jsx", "mjs", "cjs"],
        "ts" | "typescript" => &["ts", "tsx", "mts", "cts"],
        "java" => &["java"],
        "kotlin" | "kt" => &["kt", "kts"],
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
        "cs" | "csharp" => &["cs"],
        "scala" => &["scala"],
        "sql" => &["sql"],
        "html" => &["html", "htm"],
        "css" => &["css", "scss", "sass", "less"],
        "vim" => &["vim"],
        _ => return None,
    };
    Some(format!(r"\.({})$", exts.join("|")))
}

/// Splits filters from the pattern. A query without filter tokens is used
/// verbatim, so spacing inside the regex is preserved.
pub fn parse(query: &str) -> Result<Parsed> {
    let mut out = Parsed::default();
    let mut rest: Vec<&str> = Vec::new();
    let mut any_filter = false;
    for tok in query.split_whitespace() {
        let (neg, body) = match tok.strip_prefix('-') {
            Some(b) if b.contains(':') => (true, b),
            _ => (false, tok),
        };
        let Some((key, val)) = body.split_once(':') else {
            rest.push(tok);
            continue;
        };
        if val.is_empty() {
            rest.push(tok);
            continue;
        }
        match key {
            "file" | "f" | "path" => {
                any_filter = true;
                if neg {
                    out.not_file.push(val.to_string())
                } else {
                    out.file.push(val.to_string())
                }
            }
            "lang" | "l" => {
                any_filter = true;
                let Some(re) = lang_regex(val) else { bail!("unknown language `{}`", val) };
                if neg {
                    out.not_file.push(re)
                } else {
                    out.file.push(re)
                }
            }
            "case" if !neg => {
                any_filter = true;
                out.case = Some(match val {
                    "yes" | "y" | "sensitive" => Case::Sensitive,
                    "no" | "n" | "insensitive" => Case::Insensitive,
                    "auto" | "smart" => Case::Smart,
                    _ => bail!("case: takes yes, no or auto"),
                });
            }
            _ => rest.push(tok),
        }
    }
    out.pattern = if any_filter { rest.join(" ") } else { query.to_string() };
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_filters() {
        let p = parse("foo bar file:src/ -file:_test lang:go case:yes").unwrap();
        assert_eq!(p.pattern, "foo bar");
        assert_eq!(p.file, vec!["src/".to_string(), r"\.(go)$".to_string()]);
        assert_eq!(p.not_file, vec!["_test".to_string()]);
        assert_eq!(p.case, Some(Case::Sensitive));
    }

    #[test]
    fn plain_query_is_verbatim() {
        assert_eq!(parse("a  b").unwrap().pattern, "a  b");
        assert_eq!(parse("std::vector").unwrap().pattern, "std::vector");
        assert_eq!(parse("http://x").unwrap().pattern, "http://x");
    }
}
