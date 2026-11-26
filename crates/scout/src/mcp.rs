//! `scout mcp`: a Model Context Protocol server over stdio, so agents get
//! scout as tools. It reads JSON-RPC messages, one per line, from stdin and
//! writes replies to stdout. Nothing listens on the network.

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Result};
use serde_json::{json, Value};

use scout_index::{build, BuildOptions, Index, Manifest};
use scout_query::{Hit, SearchOptions};

use crate::{daemon, repo, status_json};

const PROTOCOL: &str = "2025-06-18";
const MAX_LIMIT: usize = 500;
const MAX_CONTEXT: usize = 10;
const LINE_CAP: usize = 240;

const QUERY_HELP: &str = "A regex (smart case) with optional filters: file:RE, -file:RE, lang:NAME (c, cpp, go, rust, python, \
js, ts, lua, bazel, ...), case:yes. Examples: `kmalloc\\(.*GFP_ATOMIC lang:c`, `TODO file:^src/ -file:_test`.";

fn tools() -> Value {
    let filters = json!({
        "type": "string",
        "description": "Optional filters, e.g. `file:^src/net/ lang:cpp -file:_test`."
    });
    let limit = json!({"type": "integer", "description": "Maximum results (default 50, at most 500).", "minimum": 1});
    let context = json!({"type": "integer", "description": "Lines of context before and after each hit (default 0, at most 10).", "minimum": 0});
    json!([
        {
            "name": "search",
            "title": "Search code",
            "description": format!("Indexed regex search over the whole repository (tracked, untracked and hidden files; .gitignored files skipped). Much faster than grep on large repos. {} Results are path, line and the matching line, grouped by file and best first.", QUERY_HELP),
            "inputSchema": {
                "type": "object",
                "properties": {"query": {"type": "string", "description": QUERY_HELP}, "limit": limit, "context": context},
                "required": ["query"]
            },
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "find_symbol",
            "title": "Find definitions",
            "description": "Finds definitions by name: functions, methods, classes, structs, types, macros, constants and Bazel targets, from tree-sitter at index time (C, C++, Python, Starlark, Rust, Go, Lua, JS, TS). Exact matches first, then names containing it. A regex name works too. Name based only: same-named symbols in different classes all match.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "Symbol name, e.g. `ParseConfig`, `Foo::bar` (matches bar), or a regex."},
                    "kind": {"type": "string", "description": "Optional: function, method, class, struct, enum, type, interface, macro, constant, target, prototype."},
                    "filters": filters.clone(),
                    "limit": limit.clone(),
                    "context": context.clone()
                },
                "required": ["name"]
            },
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "find_callers",
            "title": "Find callers",
            "description": "Finds call sites of a function or method by name, with the function each call is in. Approximate: matched by name, no type resolution, so calls to same-named functions mix.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "Function or method name, e.g. `kmalloc` or `Widget::Draw` (matches Draw)."},
                    "filters": filters,
                    "limit": limit,
                    "context": context
                },
                "required": ["name"]
            },
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "index_status",
            "title": "Index status",
            "description": "Shows whether the repository is indexed, how many files, whether symbols are ready, and whether the background updater is running.",
            "inputSchema": {"type": "object", "properties": {}},
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "reindex",
            "title": "Update the index",
            "description": "Brings the index up to date. Builds it if there is none (can take minutes on a large repo), otherwise re-checks changed files. full=true rebuilds from scratch.",
            "inputSchema": {
                "type": "object",
                "properties": {"full": {"type": "boolean", "description": "Rebuild from scratch."}}
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true}
        }
    ])
}

struct Server {
    root: PathBuf,
    dir: PathBuf,
}

fn arg_str<'a>(args: &'a Value, k: &str) -> Option<&'a str> {
    args.get(k).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty())
}

fn arg_usize(args: &Value, k: &str, default: usize, max: usize) -> usize {
    args.get(k).and_then(|v| v.as_u64()).map(|n| n as usize).unwrap_or(default).min(max)
}

fn cut(line: &[u8]) -> String {
    let s = String::from_utf8_lossy(line);
    let s = s.trim_end();
    if s.chars().count() > LINE_CAP {
        let t: String = s.chars().take(LINE_CAP).collect();
        format!("{t}…")
    } else {
        s.to_string()
    }
}

impl Server {
    fn ensure_daemon(&self) {
        if Manifest::exists(&self.dir) && !daemon::is_running(&self.dir) {
            let _ = daemon::spawn_detached(&self.root, &self.dir, &None, Duration::from_secs(30 * 60));
        }
    }

    fn run_query(&self, query: &str, limit: usize, context: usize) -> Result<String> {
        let opts = SearchOptions { max_matches: limit, ..Default::default() };
        let mut hits: Vec<Hit> = Vec::new();
        let mut emit = |h: &Hit| {
            hits.push(h.clone());
            true
        };
        if !Manifest::exists(&self.dir) {
            // Build in the background and scan meanwhile.
            crate::start_background_build(&self.root, &self.dir);
            let stats = scout_query::search_unindexed(&self.root, query, &opts, &mut emit)?;
            let mut out = self.format(&hits, context, stats.truncated, false);
            out.push_str("\nindex_status: building. The index is being built in the background; this answer came from scanning files, and later searches will use the index once it's ready.");
            return Ok(out);
        }
        self.ensure_daemon();
        let index = Index::open(&self.dir)?;
        let stats = scout_query::search(&index, &self.root, query, &opts, &mut emit)?;
        Ok(self.format(&hits, context, stats.truncated, stats.symbols_incomplete))
    }

    /// Groups hits by file, keeping rank order of first appearance.
    fn format(&self, hits: &[Hit], context: usize, truncated: bool, incomplete: bool) -> String {
        if hits.is_empty() {
            let mut s = "No results.".to_string();
            if incomplete {
                s.push_str(" Symbols are still being indexed; try again shortly or use search.");
            }
            return s;
        }
        let mut order: Vec<&str> = Vec::new();
        let mut by_file: HashMap<&str, Vec<&Hit>> = HashMap::new();
        for h in hits {
            by_file
                .entry(h.path.as_str())
                .or_insert_with(|| {
                    order.push(h.path.as_str());
                    Vec::new()
                })
                .push(h);
        }
        let mut out = String::new();
        for path in order {
            let mut hs = by_file.remove(path).unwrap_or_default();
            // Text hits read best in file order; symbol hits keep rank order.
            if hs.iter().all(|h| h.kind.is_none()) {
                hs.sort_by_key(|h| h.line);
            }
            out.push_str(path);
            out.push('\n');
            let lines: Vec<Vec<u8>> = if context > 0 {
                fs::read(self.root.join(path))
                    .map(|d| d.split(|&b| b == b'\n').map(|l| l.to_vec()).collect())
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let mut last = 0u64;
            for h in hs {
                if context > 0 {
                    let back = h.line <= last;
                    let from = h.line.saturating_sub(context as u64).max(1);
                    let from = if back { from } else { from.max(last + 1) };
                    if last > 0 && (back || from > last + 1) {
                        out.push_str("  ...\n");
                    }
                    for n in from..h.line {
                        if let Some(l) = lines.get(n as usize - 1) {
                            out.push_str(&format!("  {}- {}\n", n, cut(l)));
                        }
                    }
                }
                let note = match (h.kind, &h.container) {
                    (Some("call"), Some(c)) => format!("  (in {c})"),
                    (Some("call"), None) => String::new(),
                    (Some(k), _) => format!("  ({k})"),
                    _ => String::new(),
                };
                out.push_str(&format!("  {}: {}{}\n", h.line, cut(&h.text), note));
                last = h.line;
                if context > 0 {
                    let to = h.line + context as u64;
                    for n in h.line + 1..=to {
                        if let Some(l) = lines.get(n as usize - 1) {
                            out.push_str(&format!("  {}- {}\n", n, cut(l)));
                            last = n;
                        }
                    }
                }
            }
        }
        let files = hits.iter().map(|h| h.path.as_str()).collect::<std::collections::HashSet<_>>().len();
        out.push_str(&format!(
            "\n{} result{} in {} file{}{}.",
            hits.len(),
            if hits.len() == 1 { "" } else { "s" },
            files,
            if files == 1 { "" } else { "s" },
            if truncated { "; more exist, raise limit or narrow with filters" } else { "" }
        ));
        if incomplete {
            out.push_str(" Symbols are still being indexed, so this may be incomplete.");
        }
        out
    }

    fn reindex(&self, full: bool) -> Result<String> {
        if !Manifest::exists(&self.dir) || (full && !daemon::is_running(&self.dir)) {
            let opts = BuildOptions { commit: repo::head(&self.root), ..Default::default() };
            let st = build::build_full(&self.root, &self.dir, &opts)?;
            self.ensure_daemon();
            return Ok(format!(
                "Indexed {} files in {:.1?}. Text search is ready; symbols (find_symbol, find_callers) are being indexed in the background.",
                st.files_indexed, st.total_time
            ));
        }
        if !daemon::is_running(&self.dir) {
            self.ensure_daemon();
            return Ok("Started the background updater; it re-checks changed files now and keeps the index fresh.".into());
        }
        let cmd = if full { "rebuild" } else { "reconcile full" };
        daemon::send(&self.dir, &[cmd.to_string()], false)?;
        Ok(if full {
            "Rebuild started in the background.".into()
        } else {
            "Re-checking every file against the index in the background; changes show up within seconds.".into()
        })
    }

    fn call(&self, name: &str, args: &Value) -> Result<String> {
        let limit = arg_usize(args, "limit", 50, MAX_LIMIT).max(1);
        let context = arg_usize(args, "context", 0, MAX_CONTEXT);
        let filters = arg_str(args, "filters").unwrap_or("");
        match name {
            "search" => {
                let Some(q) = arg_str(args, "query") else { bail!("query is required") };
                self.run_query(q, limit, context)
            }
            "find_symbol" | "find_callers" => {
                let Some(n) = arg_str(args, "name") else { bail!("name is required") };
                if n.contains(char::is_whitespace) {
                    bail!("name must be one word; put filters in `filters`");
                }
                let prefix = if name == "find_symbol" { "sym" } else { "calls" };
                let mut q = format!("{prefix}:{n} {filters}");
                if let Some(k) = arg_str(args, "kind").filter(|_| name == "find_symbol") {
                    q.push_str(&format!(" kind:{k}"));
                }
                self.run_query(q.trim(), limit, context)
            }
            "index_status" => {
                let mut v = status_json(&self.root, &self.dir);
                if let Some(o) = v.as_object_mut() {
                    o.remove("socket");
                    o.remove("index_dir");
                }
                Ok(serde_json::to_string_pretty(&v)?)
            }
            "reindex" => self.reindex(args.get("full").and_then(|v| v.as_bool()).unwrap_or(false)),
            other => bail!("unknown tool `{}`", other),
        }
    }

    fn handle(&self, msg: &Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let result: Result<Value, (i64, String)> = match method {
            "initialize" => {
                let v = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or(PROTOCOL).to_string();
                Ok(json!({
                    "protocolVersion": v,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "scout", "title": "scout code search", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": format!(
                        "scout searches the repository at {} through a local index. Prefer it over grep/rg for searching code: search for text and regexes, find_symbol for definitions, find_callers for call sites.",
                        self.root.display()
                    ),
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools()})),
            "tools/call" => {
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                Ok(match self.call(name, &args) {
                    Ok(text) => json!({"content": [{"type": "text", "text": text}], "isError": false}),
                    Err(e) => json!({"content": [{"type": "text", "text": format!("{:#}", e)}], "isError": true}),
                })
            }
            _ if id.is_none() => return None, // notifications need no reply
            _ => Err((-32601, format!("method not found: {}", method))),
        };
        let id = id?;
        Some(match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err((code, message)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
        })
    }
}

pub fn serve(root: &Path, dir: &Path) -> Result<()> {
    let server = Server { root: root.to_path_buf(), dir: dir.to_path_buf() };
    server.ensure_daemon();
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(Value::Array(batch)) => {
                let r: Vec<Value> = batch.iter().filter_map(|m| server.handle(m)).collect();
                (!r.is_empty()).then(|| Value::Array(r))
            }
            Ok(msg) => server.handle(&msg),
            Err(e) => Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {}", e)}})),
        };
        if let Some(r) = reply {
            writeln!(stdout, "{}", r)?;
            stdout.flush()?;
        }
    }
    Ok(())
}
