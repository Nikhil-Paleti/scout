//! scout: indexed code search for big repositories.

mod args;
mod daemon;
mod output;
mod priority;
mod repo;

use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::json;

use args::Args;
use scout_index::manifest::{self, index_dir_for, Manifest};
use scout_index::{build, filter, BuildOptions, Index, Rules, Verdict};
use output::{Format, Printer};
use scout_query::{Case, SearchOptions};

const USAGE: &str = "\
scout: indexed code search

USAGE:
  scout [search] [OPTIONS] QUERY      search the index (see QUERY below)
  scout sym NAME [FILTERS]            definitions, same as `scout 'sym:NAME'`
  scout calls NAME [FILTERS]          call sites, same as `scout 'calls:NAME'`
  scout refs NAME [FILTERS]           whole-word uses, same as `scout 'refs:NAME'`
  scout build   [--root DIR] [--exclude GLOB]... [--threads N] [--mem-mb N] [--no-nice] [--no-symbols] [--quiet]
  scout files   [--root DIR]
  scout serve   [--root DIR] [--foreground] [--exclude GLOB]... [--idle-secs N]
  scout notify  [--root DIR] [--start] PATH...
  scout reconcile [--root DIR] [--full]
  scout status  [--root DIR] [--json] [--why PATH]
  scout stop    [--root DIR]
  scout version

SEARCH OPTIONS:
  --json            one JSON object per hit, then a summary object (for tools and agents)
  --vimgrep         path:line:col:text, no color or annotations
  -l, --files       matching file names only
  -C/-A/-B N        context lines around (after, before) each hit
  -m, --max N       stop after N hits (default 5000)
  -F, -w, -s, -i    fixed string, whole word, case sensitive, ignore case
  --near PATH       rank results near this file first
  --color WHEN      always, never or auto (default; honors NO_COLOR)
  --root DIR        repository root (default: git root of the current directory)
  --stats           print timing to stderr

QUERY is a regex (smart case) with optional filters:
  file:RE  -file:RE  lang:NAME  -lang:NAME  case:yes|no|auto
  sym:NAME     definitions (functions, methods, types, macros, Bazel targets)
  calls:NAME   call sites of NAME, with the function each call is in
  refs:NAME    whole-word, case-sensitive matches of NAME
  kind:K       with sym:/calls:, e.g. kind:function, kind:class, kind:target
Symbols are matched by name only (no type resolution): same-named functions mix.

Exit status: 0 hits, 1 no hits, 2 error.
";

fn main() -> ExitCode {
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.is_empty() || matches!(argv[0].as_str(), "-h" | "--help" | "help") {
        print!("{}", USAGE);
        return ExitCode::SUCCESS;
    }
    let cmd = argv.remove(0);
    let all: Vec<String> = std::iter::once(cmd.clone()).chain(argv.iter().cloned()).collect();
    let args = Args::new(argv);
    let r = match cmd.as_str() {
        "build" => cmd_build(args),
        "search" | "s" => cmd_search(args, ""),
        "sym" => cmd_search(args, "sym:"),
        "calls" | "callers" => cmd_search(args, "calls:"),
        "refs" => cmd_search(args, "refs:"),
        "files" => cmd_files(args),
        "serve" => cmd_serve(args),
        "notify" => cmd_notify(args),
        "reconcile" => cmd_reconcile(args),
        "status" => cmd_status(args),
        "stop" => cmd_stop(args),
        "version" | "--version" | "-V" => {
            println!("scout {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        // `scout QUERY` is short for `scout search QUERY`.
        _ => cmd_search(Args::new(all), ""),
    };
    match r {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            if is_broken_pipe(&e) {
                return ExitCode::SUCCESS;
            }
            eprintln!("scout: {:#}", e);
            ExitCode::from(2)
        }
    }
}

fn is_broken_pipe(e: &anyhow::Error) -> bool {
    e.chain()
        .any(|c| c.downcast_ref::<io::Error>().map(|e| e.kind() == io::ErrorKind::BrokenPipe).unwrap_or(false))
}

fn root_and_dir(args: &mut Args) -> Result<(PathBuf, PathBuf)> {
    let root = repo::find_root(args.opt(&["--root"])?.as_deref())?;
    let dir = index_dir_for(&root);
    Ok((root, dir))
}

fn cmd_build(mut a: Args) -> Result<u8> {
    let (root, dir) = root_and_dir(&mut a)?;
    let excludes = a.opts(&["--exclude"])?;
    let no_excludes = a.flag(&["--no-excludes"]);
    let threads: Option<usize> = a.parsed(&["--threads", "-j"])?;
    let mem_mb: Option<usize> = a.parsed(&["--mem-mb"])?;
    let no_nice = a.flag(&["--no-nice"]);
    let quiet = a.flag(&["--quiet", "-q"]);
    let json_out = a.flag(&["--json"]);
    let no_symbols = a.flag(&["--no-symbols"]);
    a.positionals()?;
    if !no_nice {
        priority::background();
    }
    let excludes = if !excludes.is_empty() || no_excludes {
        excludes
    } else {
        Manifest::load(&dir).map(|m| m.excludes).unwrap_or_default()
    };
    let opts = BuildOptions {
        threads: threads.unwrap_or_else(build::default_threads),
        mem_budget: mem_mb.unwrap_or(512) << 20,
        excludes,
        commit: repo::head(&root),
        progress: !quiet && !json_out,
        symbols: !no_symbols,
        ..Default::default()
    };
    let st = build::build_full(&root, &dir, &opts)?;
    if json_out {
        println!(
            "{}",
            json!({
                "root": root, "index_dir": dir, "files": st.files_indexed, "bytes": st.bytes_indexed,
                "index_bytes": st.index_bytes, "trigrams": st.trigrams, "batches": st.batches,
                "symbols": st.symbols, "sym_bytes": st.sym_bytes, "sym_cpu_ms": st.sym_cpu.as_millis() as u64,
                "skipped": {"large": st.skipped_large, "binary": st.skipped_binary,
                            "minified": st.skipped_minified, "unreadable": st.skipped_unreadable},
                "walk_ms": st.walk_time.as_millis() as u64, "total_ms": st.total_time.as_millis() as u64,
            })
        );
    } else if !quiet {
        eprintln!(
            "scout: indexed {} files ({:.1} MB) in {:.1?}; index {:.1} MB ({:.0}% of text), {} trigrams, {} batch(es)",
            st.files_indexed,
            st.bytes_indexed as f64 / 1e6,
            st.total_time,
            st.index_bytes as f64 / 1e6,
            100.0 * st.index_bytes as f64 / st.bytes_indexed.max(1) as f64,
            st.trigrams,
            st.batches
        );
        if opts.symbols {
            eprintln!("scout: text search is ready; indexing symbols (sym:, calls:) next");
        }
        eprintln!(
            "scout: skipped {} large, {} binary, {} minified, {} unreadable",
            st.skipped_large, st.skipped_binary, st.skipped_minified, st.skipped_unreadable
        );
    }
    if opts.symbols {
        let cancel = std::sync::atomic::AtomicBool::new(false);
        match scout_index::symbolize::backfill(&root, &dir, opts.threads, !quiet && !json_out, &cancel)? {
            Some(ss) => {
                if json_out {
                    println!(
                        "{}",
                        json!({"symbols": ss.symbols, "sym_bytes": ss.bytes, "sym_files": ss.files,
                               "sym_ms": ss.time.as_millis() as u64, "sym_cpu_ms": ss.cpu.as_millis() as u64})
                    );
                } else if !quiet {
                    eprintln!(
                        "scout: {} symbols from {} files in {:.1?} ({:.1?} CPU); {:.1} MB",
                        ss.symbols,
                        ss.files,
                        ss.time,
                        ss.cpu,
                        ss.bytes as f64 / 1e6
                    );
                }
            }
            None if !quiet => eprintln!("scout: the daemon is indexing symbols"),
            None => {}
        }
    }
    // Tell a running daemon to pick up anything that changed during the build.
    if daemon::is_running(&dir) {
        let _ = daemon::send(&dir, &["reconcile".to_string()], false);
    }
    Ok(0)
}

fn open_index(dir: &Path, root: &Path) -> Result<Index> {
    if !Manifest::exists(dir) {
        bail!("no index for {}; run `scout build` (or :ScoutIndex)", root.display());
    }
    Index::open(dir)
}

fn cmd_search(mut a: Args, prefix: &str) -> Result<u8> {
    let (root, dir) = root_and_dir(&mut a)?;
    let json_out = a.flag(&["--json"]);
    let files_out = a.flag(&["--files-with-matches", "--files", "-l"]);
    let vimgrep = a.flag(&["--vimgrep"]);
    let fixed = a.flag(&["--fixed-strings", "-F"]);
    let word = a.flag(&["--word-regexp", "-w"]);
    let sensitive = a.flag(&["--case-sensitive", "-s"]);
    let insensitive = a.flag(&["--ignore-case", "-i"]);
    let no_filters = a.flag(&["--no-filters"]);
    let show_stats = a.flag(&["--stats"]);
    let max: Option<usize> = a.parsed(&["--max-count", "--max", "-m"])?;
    let threads: Option<usize> = a.parsed(&["--threads", "-j"])?;
    let ctx: Option<usize> = a.parsed(&["--context", "-C"])?;
    let before: Option<usize> = a.parsed(&["--before-context", "-B"])?;
    let after: Option<usize> = a.parsed(&["--after-context", "-A"])?;
    let color = a.opt(&["--color"])?;
    let near = a.opt(&["--near"])?;
    let pos = a.positionals()?;
    if pos.is_empty() {
        bail!("search needs a QUERY");
    }
    let query = format!("{}{}", prefix, pos.join(" "));
    let mut opts = SearchOptions {
        fixed,
        word,
        filters: !no_filters,
        case: if sensitive {
            Case::Sensitive
        } else if insensitive {
            Case::Insensitive
        } else {
            Case::Smart
        },
        ..Default::default()
    };
    if let Some(m) = max {
        opts.max_matches = m;
    }
    if let Some(t) = threads {
        opts.threads = t;
    }
    opts.near = near.map(|n| {
        let p = PathBuf::from(&n);
        let p = if p.is_absolute() { p } else { std::env::current_dir().map(|c| c.join(&p)).unwrap_or(p) };
        p.strip_prefix(&root).map(|r| r.display().to_string()).unwrap_or(n)
    });
    let format = if json_out {
        Format::Json
    } else if files_out {
        Format::Files
    } else if vimgrep {
        Format::Vimgrep
    } else {
        Format::Text
    };
    let color = match color.as_deref() {
        Some("always") => true,
        Some("never") => false,
        Some("auto") | None => output::stdout_is_tty() && std::env::var_os("NO_COLOR").is_none(),
        Some(other) => bail!("--color takes always, never or auto, not `{}`", other),
    };
    let b = before.or(ctx).unwrap_or(0);
    let af = after.or(ctx).unwrap_or(0);

    let index = open_index(&dir, &root)?;
    let stdout = io::stdout();
    let out = BufWriter::with_capacity(64 * 1024, stdout.lock());
    let mut pr = Printer::new(out, format, color, b, af, &root);
    let mut err: Option<io::Error> = None;
    let stats = scout_query::search(&index, &root, &query, &opts, &mut |h| {
        // Flush early so a picker shows the first results right away.
        let r = pr.hit(h).and_then(|_| if pr.hits == 1 || pr.hits % 200 == 0 { pr.flush() } else { Ok(()) });
        if let Err(e) = r {
            err = Some(e);
            return false;
        }
        true
    })?;
    if let Some(e) = err {
        return Err(e.into());
    }
    pr.flush_after(None)?;
    pr.summary(&stats)?;
    pr.flush()?;
    if stats.symbols_incomplete && format != Format::Json {
        eprintln!("scout: symbols are still being indexed; results may be incomplete");
    }
    if show_stats {
        eprintln!(
            "scout: plan {}\nscout: {} segments, {} live files, {} candidates, {} searched, {} matched, {} matches{}\nscout: plan {:.2?}, candidates {:.2?}, first hit {}, total {:.2?}",
            stats.plan,
            stats.segments,
            stats.live_files,
            stats.candidates,
            stats.files_searched,
            stats.files_matched,
            stats.matches,
            if stats.truncated { " (capped)" } else { "" },
            stats.plan_time,
            stats.candidate_time,
            stats.first_hit.map(|d| format!("{:.2?}", d)).unwrap_or_else(|| "-".into()),
            stats.total_time
        );
    }
    Ok(if stats.matches > 0 { 0 } else { 1 })
}

fn cmd_files(mut a: Args) -> Result<u8> {
    let (root, dir) = root_and_dir(&mut a)?;
    a.positionals()?;
    let index = open_index(&dir, &root)?;
    let mut paths: Vec<&[u8]> = index.live_files().map(|f| f.path).collect();
    paths.sort();
    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    for p in paths {
        out.write_all(p)?;
        out.write_all(b"\n")?;
    }
    out.flush()?;
    Ok(0)
}

fn serve_excludes(a: &mut Args) -> Result<Option<Vec<String>>> {
    let ex = a.opts(&["--exclude"])?;
    let none = a.flag(&["--no-excludes"]);
    Ok(if !ex.is_empty() || none { Some(ex) } else { None })
}

fn cmd_serve(mut a: Args) -> Result<u8> {
    let (root, dir) = root_and_dir(&mut a)?;
    let foreground = a.flag(&["--foreground"]);
    let excludes = serve_excludes(&mut a)?;
    let idle = Duration::from_secs(a.parsed(&["--idle-secs"])?.unwrap_or(1800));
    let threads: Option<usize> = a.parsed(&["--threads", "-j"])?;
    a.positionals()?;
    if foreground {
        daemon::serve(
            &root,
            &dir,
            daemon::ServeOptions { idle, excludes, threads: threads.unwrap_or_else(build::default_threads) },
        )?;
    } else {
        daemon::spawn_detached(&root, &dir, &excludes, idle)?;
    }
    Ok(0)
}

fn cmd_notify(mut a: Args) -> Result<u8> {
    let (root, dir) = root_and_dir(&mut a)?;
    let start = a.flag(&["--start"]);
    let paths = a.positionals()?;
    let lines: Vec<String> = paths
        .iter()
        .map(|p| {
            let pb = PathBuf::from(p);
            let abs = if pb.is_absolute() { pb } else { std::env::current_dir().unwrap_or_default().join(pb) };
            format!("notify {}", abs.display())
        })
        .collect();
    if daemon::send(&dir, &lines, false).is_err() && start {
        daemon::spawn_detached(&root, &dir, &None, Duration::from_secs(1800))?;
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(100));
            if daemon::send(&dir, &lines, false).is_ok() {
                break;
            }
        }
    }
    Ok(0)
}

fn cmd_reconcile(mut a: Args) -> Result<u8> {
    let (_root, dir) = root_and_dir(&mut a)?;
    let full = a.flag(&["--full"]);
    a.positionals()?;
    let line = if full { "reconcile full" } else { "reconcile" };
    daemon::send(&dir, &[line.to_string()], false)?;
    Ok(0)
}

fn cmd_stop(mut a: Args) -> Result<u8> {
    let (_root, dir) = root_and_dir(&mut a)?;
    a.positionals()?;
    let _ = daemon::send(&dir, &["shutdown".to_string()], false);
    Ok(0)
}

fn cmd_status(mut a: Args) -> Result<u8> {
    let (root, dir) = root_and_dir(&mut a)?;
    let json_out = a.flag(&["--json"]);
    let why = a.opt(&["--why"])?;
    a.positionals()?;

    if let Some(p) = why {
        let pb = PathBuf::from(&p);
        let abs = if pb.is_absolute() { pb } else { std::env::current_dir()?.join(pb) };
        let abs = abs.canonicalize().unwrap_or(abs);
        let rel = abs.strip_prefix(&root).with_context(|| format!("{} is outside {}", p, root.display()))?;
        let m = Manifest::load(&dir).ok();
        let rules = Rules::new(&root, m.as_ref().map(|m| m.excludes.as_slice()).unwrap_or(&[]))?;
        let verdict = match rules.check(rel, abs.is_dir()) {
            Verdict::Excluded(why) => format!("skipped: {}", why),
            Verdict::Included if abs.is_dir() => "included (directory)".to_string(),
            Verdict::Included => match std::fs::read(&abs) {
                Err(e) => format!("unreadable: {}", e),
                Ok(d) => match filter::check_size(d.len() as u64, filter::DEFAULT_MAX_FILE_SIZE)
                    .or_else(|| filter::check_content(&d))
                {
                    Some(s) => format!("skipped: {}", s.describe(filter::DEFAULT_MAX_FILE_SIZE)),
                    None => "included".to_string(),
                },
            },
        };
        let indexed = Index::open(&dir)
            .ok()
            .map(|i| i.lookup(rel.as_os_str().as_encoded_bytes()).is_some())
            .unwrap_or(false);
        if json_out {
            println!("{}", json!({"path": rel, "verdict": verdict, "in_index": indexed}));
        } else {
            println!("{}: {}{}", rel.display(), verdict, if indexed { " (in index)" } else { " (not in index)" });
        }
        return Ok(0);
    }

    let index = Index::open(&dir).ok();
    let head = repo::head(&root);
    let daemon_status = daemon::send(&dir, &["status".to_string()], true)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    let socket = daemon::socket_path(&dir);
    if json_out {
        let v = match &index {
            None => json!({
                "root": root, "index_dir": dir, "indexed": false, "head": head,
                "daemon": daemon_status, "socket": socket,
            }),
            Some(i) => {
                let m = &i.manifest;
                json!({
                    "root": root, "index_dir": dir, "indexed": true, "socket": socket,
                    "files": i.live_count(), "segments": m.segments.len(),
                    "deltas": m.segments.iter().filter(|s| s.kind == "delta").count(),
                    "index_bytes": i.disk_bytes(), "commit": m.commit, "head": head,
                    "stale_commit": head.is_some() && m.commit != head,
                    "excludes": m.excludes, "built_at": m.built_at, "updated_at": m.updated_at,
                    "generation": m.generation, "daemon": daemon_status,
                })
            }
        };
        println!("{}", v);
        return Ok(0);
    }
    println!("root:      {}", root.display());
    println!("index dir: {}", dir.display());
    match &index {
        None => println!("index:     none (run `scout build`)"),
        Some(i) => {
            let m = &i.manifest;
            let deltas = m.segments.iter().filter(|s| s.kind == "delta").count();
            println!("files:     {}", i.live_count());
            println!("segments:  {} ({} delta)", m.segments.len(), deltas);
            println!("size:      {:.1} MB", i.disk_bytes() as f64 / 1e6);
            if !m.excludes.is_empty() {
                println!("excludes:  {}", m.excludes.join(", "));
            }
            let age = manifest::now_secs().saturating_sub(m.updated_at);
            println!("updated:   {}s ago", age);
            match (&m.commit, &head) {
                (Some(c), Some(h)) if c == h => println!("commit:    {} (current)", &c[..c.len().min(12)]),
                (Some(c), Some(h)) => println!(
                    "commit:    {} (HEAD is {}, reconcile pending)",
                    &c[..c.len().min(12)],
                    &h[..h.len().min(12)]
                ),
                _ => {}
            }
        }
    }
    match daemon_status {
        Some(d) => println!(
            "daemon:    running, pid {}, {} pending",
            d["pid"],
            d["pending"].as_u64().unwrap_or(0)
        ),
        None => println!("daemon:    not running"),
    }
    Ok(0)
}
