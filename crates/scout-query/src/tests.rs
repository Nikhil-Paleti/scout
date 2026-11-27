use std::fs;
use std::path::Path;

use scout_index::{build_full, BuildOptions, Index};

use super::*;
use grep_regex::RegexMatcherBuilder;

fn write(root: &Path, rel: &str, data: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, data).unwrap();
}

fn run(index: &Index, root: &Path, q: &str, opts: &SearchOptions) -> Vec<String> {
    let mut out = Vec::new();
    search(index, root, q, opts, &mut |h| {
        out.push(format!(
            "{}:{}:{}:{}",
            h.path,
            h.line,
            h.col,
            String::from_utf8_lossy(&h.text)
        ));
        true
    })
    .unwrap();
    out.sort();
    out
}

/// Brute-force oracle: every line of every indexed file the regex hits.
fn oracle(
    root: &Path,
    files: &[&str],
    re: &str,
    path_re: Option<&str>,
    sensitive: Option<bool>,
) -> Vec<String> {
    let ins = match sensitive {
        Some(s) => !s,
        None => !has_upper(re),
    };
    let m = RegexMatcherBuilder::new()
        .case_insensitive(ins)
        .multi_line(true)
        .line_terminator(Some(b'\n'))
        .build(re)
        .unwrap();
    let pm = path_re.map(|p| {
        RegexMatcherBuilder::new()
            .case_insensitive(true)
            .build(p)
            .unwrap()
    });
    let mut out = Vec::new();
    for f in files {
        if let Some(pm) = &pm {
            if !pm.is_match(f.as_bytes()).unwrap() {
                continue;
            }
        }
        let data = fs::read(root.join(f)).unwrap();
        for (i, line) in data.split(|&b| b == b'\n').enumerate() {
            if let Ok(Some(mt)) = m.find(line) {
                out.push(format!(
                    "{}:{}:{}:{}",
                    f,
                    i + 1,
                    mt.start() + 1,
                    String::from_utf8_lossy(line)
                ));
            }
        }
    }
    out.sort();
    out
}

const FILES: [(&str, &str); 6] = [
    (
        "src/main.rs",
        "fn main() {\n    println!(\"Hello, World\");\n    let colour = 1;\n}\n",
    ),
    (
        "src/lib.rs",
        "pub fn parse_config(path: &str) -> Config {\n    todo!()\n}\n// color handling\n",
    ),
    (
        "src/util_test.go",
        "func TestParseConfig(t *testing.T) {\n\tparseConfig(\"x\")\n}\n",
    ),
    (
        "README.md",
        "# Hello\nParse_Config docs. HELLO again.\nfoo.bar baz\n",
    ),
    (
        "a/b/deep.py",
        "def hello_world():\n    return 'hello' + \"world\"\n",
    ),
    (".github/ci.yml", "on: push\njobs: build\n"),
];

fn fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    tempfile::TempDir,
    Index,
) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    for (p, d) in FILES {
        write(&root, p, d);
    }
    let idx = tempfile::tempdir().unwrap();
    build_full(
        &root,
        idx.path(),
        &BuildOptions {
            threads: 2,
            ..Default::default()
        },
    )
    .unwrap();
    let index = Index::open(idx.path()).unwrap();
    (dir, root, idx, index)
}

#[test]
fn matches_brute_force() {
    let (_d, root, _i, index) = fixture();
    let names: Vec<&str> = FILES.iter().map(|f| f.0).collect();
    // Raw regex mode (no query syntax, contents only), smart case: the
    // trigram plan must never drop a real match.
    let raw = SearchOptions {
        threads: 2,
        filters: false,
        paths: false,
        case: Case::Smart,
        ..Default::default()
    };
    let queries = [
        "hello",
        "Hello",
        "parse_?config",
        "colou?r",
        r"fn \w+\(",
        "hello.*world",
        "a.b",
        r"\w+",
        "push|build",
        "(?i)HELLO",
        "jobs: build",
        "nothing_matches_this",
        "[Hh]ello, [Ww]orld",
        "^def ",
        r"\bfoo\b",
    ];
    for q in queries {
        assert_eq!(
            run(&index, &root, q, &raw),
            oracle(&root, &names, q, None, None),
            "query {:?}",
            q
        );
    }
    // Filters, contents only, Google's default case (insensitive).
    let opts = SearchOptions {
        threads: 2,
        paths: false,
        ..Default::default()
    };
    let filtered = [
        ("hello file:\\.py$", "hello", Some("\\.py$"), None),
        ("config lang:go", "config", Some("\\.go$"), None),
        ("Config case:yes", "Config", None, Some(true)),
        ("HELLO", "hello", None, Some(false)),
        ("\"jobs: build\"", "jobs: build", None, None),
    ];
    for (q, re, p, sens) in filtered {
        assert_eq!(
            run(&index, &root, q, &opts),
            oracle(&root, &names, re, p, sens.or(Some(false))),
            "query {:?}",
            q
        );
    }
    let mut got = run(&index, &root, "hello -file:README", &opts);
    got.retain(|l| l.starts_with("README"));
    assert!(got.is_empty());

    // Literal and word modes.
    let fixed = SearchOptions {
        fixed: true,
        ..raw.clone()
    };
    assert_eq!(
        run(&index, &root, "foo.bar", &fixed),
        vec!["README.md:3:1:foo.bar baz".to_string()]
    );
    let word = SearchOptions {
        word: true,
        ..raw.clone()
    };
    assert_eq!(
        run(&index, &root, "hello", &word).len(),
        oracle(&root, &names, r"\bhello\b", None, None).len()
    );
}

fn hits(index: &Index, root: &Path, q: &str, opts: &SearchOptions) -> Vec<String> {
    let mut out = Vec::new();
    search(index, root, q, opts, &mut |h| {
        out.push(if h.is_file() {
            format!("file {}", h.path)
        } else {
            format!("{}:{}", h.path, h.line)
        });
        true
    })
    .unwrap();
    out
}

#[test]
fn google_semantics() {
    let (_d, root, _i, index) = fixture();
    let opts = SearchOptions {
        threads: 2,
        ..Default::default()
    };
    // A plain term matches paths too: file hits come first.
    let h = hits(&index, &root, "main", &opts);
    assert_eq!(h[0], "file src/main.rs");
    assert!(h.contains(&"src/main.rs:1".to_string()));
    assert!(
        h.iter().position(|x| x.starts_with("file")).unwrap()
            < h.iter().position(|x| !x.starts_with("file")).unwrap()
    );
    // file:-only queries list paths.
    assert_eq!(
        hits(&index, &root, "file:util_test", &opts),
        vec!["file src/util_test.go"]
    );
    assert_eq!(
        hits(&index, &root, "lang:py", &opts),
        vec!["file a/b/deep.py"]
    );
    let mut all_src = hits(&index, &root, "f:^src/ -file:_test", &opts);
    all_src.sort();
    assert_eq!(all_src, vec!["file src/lib.rs", "file src/main.rs"]);
    // content: excludes file names.
    assert!(hits(&index, &root, "content:main", &opts)
        .iter()
        .all(|x| !x.starts_with("file")));
    // Implicit AND per file: both terms in the same file, lines for each.
    let mut h = hits(&index, &root, "content:hello content:world", &opts);
    h.sort();
    assert_eq!(h, vec!["a/b/deep.py:1", "a/b/deep.py:2", "src/main.rs:2"]);
    // OR and grouping.
    let mut h = hits(&index, &root, "(colour OR color) lang:rust", &opts);
    h.sort();
    assert_eq!(h, vec!["src/lib.rs:4", "src/main.rs:3"]);
    // Exclusion of contents.
    assert_eq!(
        hits(&index, &root, "content:hello -world", &opts),
        vec!["README.md:1", "README.md:2"]
    );
    // A term that matches only the path still lets the file in, with no lines.
    assert_eq!(
        hits(&index, &root, "deep content:return", &opts),
        vec!["a/b/deep.py:2"]
    );
    assert_eq!(hits(&index, &root, "deep", &opts), vec!["file a/b/deep.py"]);
    // Case: insensitive by default, case:yes for exact.
    assert_eq!(
        hits(&index, &root, "content:HELLO case:yes", &opts),
        vec!["README.md:2"]
    );
    // comment: and usage:.
    assert_eq!(
        hits(&index, &root, "comment:color", &opts),
        vec!["src/lib.rs:4"]
    );
    assert_eq!(
        hits(&index, &root, "usage:hello lang:py", &opts),
        vec!["a/b/deep.py:1"]
    );
    // pcre:yes matches across lines.
    assert_eq!(
        hits(&index, &root, r"pcre:yes \{\s+println", &opts),
        vec!["src/main.rs:1"]
    );
    assert!(hits(&index, &root, r"\{\s+println", &opts).is_empty());
    // paths off (rg style): no file hits.
    let rg = SearchOptions {
        paths: false,
        ..opts.clone()
    };
    assert!(hits(&index, &root, "main", &rg)
        .iter()
        .all(|x| !x.starts_with("file")));
}

#[test]
fn ranking_prefers_near_and_penalizes_tests() {
    let mut v = vec![
        (0, "vendor/x/a.rs".to_string()),
        (0, "src/foo_test.go".to_string()),
        (0, "lib/b.rs".to_string()),
        (0, "src/deep/c.rs".to_string()),
    ];
    rank(&mut v, Some("src/deep/here.rs"));
    let order: Vec<&str> = v.iter().map(|x| x.1.as_str()).collect();
    assert_eq!(
        order,
        vec![
            "src/deep/c.rs",
            "lib/b.rs",
            "src/foo_test.go",
            "vendor/x/a.rs"
        ]
    );
}

#[test]
fn respects_match_cap() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    for i in 0..50 {
        write(&root, &format!("f{}.txt", i), "needle\nneedle\n");
    }
    let idx = tempfile::tempdir().unwrap();
    build_full(&root, idx.path(), &BuildOptions::default()).unwrap();
    let index = Index::open(idx.path()).unwrap();
    let opts = SearchOptions {
        max_matches: 7,
        paths: false,
        ..Default::default()
    };
    let mut n = 0;
    let stats = search(&index, &root, "needle", &opts, &mut |_| {
        n += 1;
        true
    })
    .unwrap();
    assert_eq!(n, 7);
    assert!(stats.truncated);
}
