use std::fs;
use std::path::Path;

use scout_index::{build_full, BuildOptions, Index};

use super::*;

fn write(root: &Path, rel: &str, data: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, data).unwrap();
}

fn run(index: &Index, root: &Path, q: &str, opts: &SearchOptions) -> Vec<String> {
    let mut out = Vec::new();
    search(index, root, q, opts, &mut |h| {
        out.push(format!("{}:{}:{}:{}", h.path, h.line, h.col, String::from_utf8_lossy(&h.text)));
        true
    })
    .unwrap();
    out.sort();
    out
}

/// Brute-force oracle: every line of every indexed file the matcher hits.
fn oracle(root: &Path, files: &[&str], q: &str, opts: &SearchOptions) -> Vec<String> {
    let c = compile(q, opts).unwrap();
    let mut out = Vec::new();
    for f in files {
        if !c.path_ok(f.as_bytes()) {
            continue;
        }
        let data = fs::read(root.join(f)).unwrap();
        for (i, line) in data.split(|&b| b == b'\n').enumerate() {
            if let Ok(Some(m)) = c.matcher.find(line) {
                out.push(format!("{}:{}:{}:{}", f, i + 1, m.start() + 1, String::from_utf8_lossy(line)));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn matches_brute_force() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let files = [
        ("src/main.rs", "fn main() {\n    println!(\"Hello, World\");\n    let colour = 1;\n}\n"),
        ("src/lib.rs", "pub fn parse_config(path: &str) -> Config {\n    todo!()\n}\n// color handling\n"),
        ("src/util_test.go", "func TestParseConfig(t *testing.T) {\n\tparseConfig(\"x\")\n}\n"),
        ("README.md", "# Hello\nParse_Config docs. HELLO again.\nfoo.bar baz\n"),
        ("a/b/deep.py", "def hello_world():\n    return 'hello' + \"world\"\n"),
        (".github/ci.yml", "on: push\njobs: build\n"),
    ];
    for (p, d) in files {
        write(&root, p, d);
    }
    let idx = tempfile::tempdir().unwrap();
    build_full(&root, idx.path(), &BuildOptions { threads: 2, ..Default::default() }).unwrap();
    let index = Index::open(idx.path()).unwrap();
    let names: Vec<&str> = files.iter().map(|f| f.0).collect();
    let opts = SearchOptions { threads: 2, ..Default::default() };

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
        "hello file:\\.py$",
        "hello -file:README",
        "config lang:go",
        "Config case:yes",
        "[Hh]ello, [Ww]orld",
        "^def ",
        r"\bfoo\b",
    ];
    for q in queries {
        let got = run(&index, &root, q, &opts);
        let want = oracle(&root, &names, q, &opts);
        assert_eq!(got, want, "query {:?}", q);
    }

    // Literal and word modes.
    let fixed = SearchOptions { fixed: true, ..opts.clone() };
    assert_eq!(run(&index, &root, "foo.bar", &fixed), vec!["README.md:3:1:foo.bar baz".to_string()]);
    let word = SearchOptions { word: true, ..opts.clone() };
    assert_eq!(run(&index, &root, "hello", &word).len(), oracle(&root, &names, r"\bhello\b", &opts).len());
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
    assert_eq!(order, vec!["src/deep/c.rs", "lib/b.rs", "src/foo_test.go", "vendor/x/a.rs"]);
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
    let opts = SearchOptions { max_matches: 7, ..Default::default() };
    let mut n = 0;
    let stats = search(&index, &root, "needle", &opts, &mut |_| {
        n += 1;
        true
    })
    .unwrap();
    assert_eq!(n, 7);
    assert!(stats.truncated);
}
