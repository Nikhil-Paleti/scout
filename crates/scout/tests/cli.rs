//! End-to-end tests of the scout binary: build, search, daemon freshness.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    cache: PathBuf,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let cache = tmp.path().join("cache");
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        Env { _tmp: tmp, root, cache }
    }

    fn scout(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_scout"));
        c.current_dir(&self.root).env("SCOUT_CACHE_DIR", &self.cache).stdin(Stdio::null());
        c
    }

    fn write(&self, rel: &str, data: &str) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, data).unwrap();
    }

    fn git(&self, args: &[&str]) {
        let ok = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "init.defaultBranch=main"])
            .args(args)
            .current_dir(&self.root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {:?}", args);
    }

    fn search(&self, q: &str) -> Vec<String> {
        let out = self.scout().args(["search", "--vimgrep", q]).output().unwrap();
        let mut v: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(|s| s.to_string()).collect();
        v.sort();
        v
    }

    fn wait_for(&self, q: &str, want: usize) -> Vec<String> {
        let t = Instant::now();
        loop {
            let r = self.search(q);
            if r.len() == want || t.elapsed() > Duration::from_secs(10) {
                return r;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn serve(&self) -> Daemon {
        let child = self
            .scout()
            .args(["serve", "--foreground", "--idle-secs", "60"])
            .stderr(fs::File::create(self.root.parent().unwrap().join("daemon.log")).unwrap())
            .spawn()
            .unwrap();
        let d = Daemon(child, self.scout());
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(10) {
            let out = self.scout().args(["status", "--json"]).output().unwrap();
            let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
            if !v["daemon"].is_null() && v["indexed"] == true {
                return d;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("daemon did not start");
    }

    fn notify(&self, rel: &str) {
        let ok = self.scout().args(["notify", rel]).status().unwrap().success();
        assert!(ok);
    }
}

struct Daemon(Child, Command);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.1.args(["stop"]).status();
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(3) {
            if let Ok(Some(_)) = self.0.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.0.kill();
    }
}

fn index_files(dir: &Path) -> usize {
    let m: serde_json::Value = serde_json::from_slice(&fs::read(dir).unwrap()).unwrap();
    m["segments"].as_array().unwrap().len()
}

#[test]
fn build_search_and_exit_codes() {
    let e = Env::new();
    e.write("src/a.rs", "fn alpha_handler() {}\n");
    e.write(".hidden/b.txt", "alpha_handler in a hidden dir\n");
    e.write(".gitignore", "out/\n");
    e.write("out/gen.rs", "fn alpha_handler() {}\n");
    let st = e.scout().args(["search", "alpha"]).output().unwrap();
    assert_eq!(st.status.code(), Some(2), "no index yet");
    assert!(e.scout().args(["build", "--quiet"]).status().unwrap().success());
    assert_eq!(
        e.search("alpha_handler"),
        vec![".hidden/b.txt:1:1:alpha_handler in a hidden dir", "src/a.rs:1:4:fn alpha_handler() {}"]
    );
    let none = e.scout().args(["search", "zzz_nothing"]).output().unwrap();
    assert_eq!(none.status.code(), Some(1));
    let why = e.scout().args(["status", "--why", "out/gen.rs"]).output().unwrap();
    let why = String::from_utf8_lossy(&why.stdout);
    assert!(why.contains("skipped") && why.contains(".gitignore"), "{}", why);
    let json = e.scout().args(["search", "--json", "handler"]).output().unwrap();
    let first: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&json.stdout).lines().next().unwrap()).unwrap();
    assert!(first["ranges"].as_array().unwrap().len() == 1);
}

#[test]
fn daemon_keeps_index_fresh() {
    let e = Env::new();
    e.git(&["init", "-q"]);
    e.write("src/a.rs", "fn one() {}\n");
    e.write("src/b.rs", "fn two() {}\n");
    e.git(&["add", "."]);
    e.git(&["commit", "-qm", "init"]);
    assert!(e.scout().args(["build", "--quiet"]).status().unwrap().success());
    let _d = e.serve();

    // A save notice makes new text searchable within a second.
    e.write("src/a.rs", "fn one() { fresh_marker_xyz(); }\n");
    let t = Instant::now();
    e.notify("src/a.rs");
    let r = e.wait_for("fresh_marker_xyz", 1);
    assert_eq!(r, vec!["src/a.rs:1:12:fn one() { fresh_marker_xyz(); }"]);
    assert!(t.elapsed() < Duration::from_secs(2), "took {:?}", t.elapsed());
    // The delta carries symbols: the new call and the old definition.
    assert_eq!(e.search("calls:fresh_marker_xyz"), vec!["src/a.rs:1:12:fn one() { fresh_marker_xyz(); }"]);
    assert_eq!(e.search("sym:one"), vec!["src/a.rs:1:4:fn one() { fresh_marker_xyz(); }"]);

    // New untracked file, then a deletion.
    e.write("notes/todo.md", "fresh_marker_xyz too\n");
    e.notify("notes/todo.md");
    assert_eq!(e.wait_for("fresh_marker_xyz", 2).len(), 2);
    fs::remove_file(e.root.join("notes/todo.md")).unwrap();
    e.notify("notes/todo.md");
    assert_eq!(e.wait_for("fresh_marker_xyz", 1).len(), 1);

    // Git activity without any notice: a commit on a new branch.
    e.git(&["checkout", "-qb", "feature"]);
    e.write("src/c.rs", "fn branch_only_fn() {}\n");
    e.git(&["add", "."]);
    e.git(&["commit", "-qm", "c"]);
    let log = e.root.parent().unwrap().join("daemon.log");
    assert_eq!(e.wait_for("branch_only_fn", 1).len(), 1, "git poll should pick up the commit:\n{}", fs::read_to_string(&log).unwrap_or_default());
    e.git(&["checkout", "-q", "main"]);
    assert_eq!(e.wait_for("branch_only_fn", 0).len(), 0, "checkout back removes it");

    // Many small updates get compacted.
    for i in 0..12 {
        e.write("src/b.rs", &format!("fn two() {{ rev_{}(); }}\n", i));
        e.notify("src/b.rs");
        e.wait_for(&format!("rev_{}\\(", i), 1);
    }
    let segs = index_files(&fs::read_dir(&e.cache).unwrap().next().unwrap().unwrap().path().join("manifest.json"));
    assert!(segs <= 9, "{} segments after 12 updates", segs);
    assert_eq!(e.search("rev_11").len(), 1);
    assert_eq!(e.search("rev_10").len(), 0);
}

#[test]
fn scoutignore_change_reindexes() {
    let e = Env::new();
    e.write(".gitignore", "gen/\n");
    e.write("gen/api.h", "int generated_api_fn(void);\n");
    assert!(e.scout().args(["build", "--quiet"]).status().unwrap().success());
    assert!(e.search("generated_api_fn").is_empty());
    let _d = e.serve();
    e.write(".scoutignore", "!gen/\n");
    e.notify(".scoutignore");
    assert_eq!(e.wait_for("generated_api_fn", 1).len(), 1);
}

#[test]
fn symbols_and_output_formats() {
    let e = Env::new();
    e.write(
        "src/widget.cc",
        "namespace ui {\nclass Widget {\n public:\n  void Draw();\n};\nvoid Widget::Draw() {\n  Paint(1);\n}\nvoid Paint(int n) {}\n}\n",
    );
    e.write("app/main.py", "from ui import paint\n\ndef main():\n    Paint(2)\n    run()\n\ndef run():\n    pass\n");
    e.write("app/BUILD", "py_binary(\n    name = \"app\",\n    srcs = [\"main.py\"],\n)\n");
    let b = e.scout().args(["build", "--quiet"]).output().unwrap();
    assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));

    let lines = |args: &[&str]| -> Vec<String> {
        let o = e.scout().args(args).output().unwrap();
        String::from_utf8_lossy(&o.stdout).lines().map(|s| s.to_string()).collect()
    };
    // Definitions: exact name first; the header-style declaration comes too.
    let defs = lines(&["sym", "Draw", "--vimgrep"]);
    assert_eq!(defs.len(), 2, "{defs:?}");
    assert!(defs.iter().any(|l| l.starts_with("src/widget.cc:6:")), "{defs:?}");
    assert_eq!(lines(&["sym:Paint kind:function", "--vimgrep"]), vec!["src/widget.cc:9:6:void Paint(int n) {}"]);
    assert_eq!(lines(&["sym", "app", "lang:bazel", "--vimgrep"]), vec!["app/BUILD:2:13:    name = \"app\","]);

    // Callers, with the enclosing function in JSON.
    let calls = lines(&["calls", "Paint", "--json"]);
    assert_eq!(calls.len(), 3, "{calls:?}");
    let v: Vec<serde_json::Value> = calls.iter().map(|l| serde_json::from_str(l).unwrap()).collect();
    let mut ins: Vec<(String, String)> = v[..2]
        .iter()
        .map(|h| (h["path"].as_str().unwrap().to_string(), h["in"].as_str().unwrap().to_string()))
        .collect();
    ins.sort();
    assert_eq!(ins, vec![("app/main.py".into(), "main".into()), ("src/widget.cc".into(), "Draw".into())]);
    assert_eq!(v[0]["kind"], "call");
    assert_eq!(v[0]["symbol"], "Paint");
    assert_eq!(v[2]["type"], "summary");
    assert_eq!(v[2]["matches"], 2);

    // refs: is whole word and case sensitive: `paint` on the import line is not a match.
    let refs = lines(&["refs", "Paint", "--vimgrep"]);
    assert_eq!(refs.len(), 3, "{refs:?}");

    // Context lines and `scout QUERY` shorthand.
    let ctx = lines(&["def run", "-C", "1", "--color", "never"]);
    assert_eq!(ctx, vec!["app/main.py-6-", "app/main.py:7:1:def run():", "app/main.py-8-    pass"]);
    let ctx = lines(&["sym:run", "-A", "1", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&ctx[0]).unwrap();
    assert_eq!(v["after"], serde_json::json!(["    pass"]));

    // Exit codes: 1 for no hits, 2 for a bad query.
    assert_eq!(e.scout().args(["calls", "Nope"]).status().unwrap().code(), Some(1));
    assert_eq!(e.scout().args(["sym:x kind:bogus"]).status().unwrap().code(), Some(2));
}
