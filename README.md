# scout

Indexed code search for big repositories, in the spirit of Google Code
Search. One Rust binary builds a trigram index of your repo, keeps it fresh
in the background, and answers regex searches in milliseconds. Use it from
the terminal, from agents over MCP, or from Neovim.

- Regex search with ripgrep-compatible output, 5 to 60 times faster than rg
  on the Linux kernel (numbers below).
- Definitions and approximate callers from tree-sitter (`sym:`, `calls:`),
  which helps where an LSP struggles, such as Bazel monorepos.
- Tracked, untracked and hidden files are searchable. `.gitignore`d files,
  `.git/`, binaries, files over 1 MB and minified files are skipped.
- Everything stays on your machine: the index lives in `~/.cache/scout`,
  the daemon listens only on a Unix socket (mode 0600), and the binary has
  no networking code.
- Linux and macOS.

## Install

```sh
git clone https://github.com/Nikhil-Paleti/scout
cd scout
cargo build --release
# put target/release/scout on your PATH, e.g.
ln -s "$PWD/target/release/scout" ~/.local/bin/scout
```

### Neovim (lazy.nvim, LazyVim)

```lua
{
  "Nikhil-Paleti/scout",
  build = "cargo build --release",
  event = "VeryLazy",
  opts = {
    route_lazyvim_keys = true, -- <leader>/, <leader>sg, <leader>sw use scout when indexed
    min_chars = 3,             -- live search waits for this many characters
    exclude = {},              -- extra gitignore-style globs to skip on this machine
  },
  keys = {
    { "<leader>sd", function() require("scout").sym() end, desc = "Definitions (scout)" },
    { "<leader>sC", function() require("scout").callers() end, desc = "Callers (scout)" },
  },
}
```

The plugin uses its own `target/release/scout`, then `scout` on `$PATH`. It
needs Neovim 0.9 or later. With snacks.nvim (LazyVim's picker) you get live
pickers; without it, results go to the quickfix list.

Commands:

| Command | What it does |
| --- | --- |
| `:ScoutLive [query]` | live search picker |
| `:Scout {query}` | results in the quickfix list |
| `:ScoutWord` | word under the cursor (or the selection) as a literal |
| `:ScoutSym [name]` | definitions of a name (default: word under the cursor) |
| `:ScoutCallers [name]` | call sites of a name (default: word under the cursor) |
| `:ScoutIndex [rebuild]` | index this project in the background, or rebuild |
| `:ScoutStatus` | files, size, symbols, commit, daemon |
| `:checkhealth scout` | binary, index and daemon checks |

Saves are sent to the background updater, and it re-checks git when Neovim
regains focus, so edits show up in results within a second. LazyVim's grep
keys go to scout only in projects that have an index; elsewhere they stay on
ripgrep.

## Command line

```sh
scout build                       # index the repo you're in (optional, see below)
scout 'kmalloc_array'             # same as `scout search ...`
scout 'TODO|FIXME' file:^src/ -file:_test lang:cpp
scout sym ParseConfig             # definitions
scout calls ParseConfig           # call sites, with the function each is in
scout refs ParseConfig            # whole-word, case-sensitive uses
scout --json -C 2 'retry_count'   # JSON lines for tools and agents
scout status
```

You don't have to build first. With no index, `scout search` starts
building one in the background (one build per repo, at low priority) and
answers right away by scanning files, with the same ignore rules and output.
A note goes to stderr, and `--json` summaries say `"index_status":
"building"`. Once the index is ready, searches use it automatically. Pass
`--no-auto-index` to get an error instead.

Output is `path:line:col:text`, colored on a terminal like rg. Useful flags:

| Flag | Meaning |
| --- | --- |
| `--json` | one JSON object per hit, then a summary object |
| `--vimgrep` | `path:line:col:text`, no color or annotations |
| `-l`, `--files` | matching file names only |
| `-C/-A/-B N` | context lines |
| `-m, --max N` | stop after N hits (default 5000) |
| `-F`, `-w`, `-s`, `-i` | fixed string, whole word, case sensitive, ignore case |
| `--near PATH` | rank results near this file first |

Exit status is 0 with hits, 1 with none and 2 on error.

### JSON output

Each hit is one line:

```json
{"type":"hit","path":"mm/damon/sysfs-common.c","line":126,"col":9,"text":"\tpath = kmalloc_array(PATH_MAX, sizeof(*path), GFP_KERNEL);","kind":"call","symbol":"kmalloc_array","in":"damon_sysfs_memcg_path_to_id","ranges":[[8,21]]}
```

`kind` is `text` for plain matches, or the symbol kind (`function`,
`class`, `target`, `call` and so on) for `sym:` and `calls:` hits. `in` is
the enclosing function of a call. `ranges` are byte offsets of matches in
`text`. With `-C`, hits also carry `before` and `after` line arrays. The
last line is a summary: `{"type":"summary","matches":..,"files":..,
"truncated":..,"elapsed_ms":..,"index_status":"ready"}`.

## Query syntax

A query is a regex (smart case: case-insensitive unless it has a capital)
plus optional filters:

| Filter | Meaning |
| --- | --- |
| `file:RE`, `-file:RE` | keep or drop paths matching RE |
| `lang:NAME`, `-lang:NAME` | by language, e.g. `lang:cpp`, `lang:starlark` |
| `case:yes`, `case:no`, `case:auto` | case sensitivity |
| `sym:NAME` | definitions: functions, methods, types, macros, constants, Bazel targets |
| `calls:NAME` | call sites of NAME, with the enclosing function |
| `refs:NAME` | whole-word, case-sensitive matches of NAME |
| `kind:K` | with `sym:` or `calls:`, e.g. `kind:function`, `kind:class`, `kind:target` |

`Foo::bar` and `Foo.bar` in `sym:` and `calls:` match `bar`. Symbols come
from tree-sitter for C, C++, Python, Starlark (BUILD, `.bzl`), Rust, Go,
Lua, JavaScript and TypeScript.

**Limitation:** symbols are matched by name only, with no type resolution,
so `calls:Draw` lists calls to every function named `Draw`. Narrow with
`file:` or `lang:` when names collide.

## Agents (MCP)

`scout mcp` is an MCP server on stdio. It has no network listener; the
agent starts it as a child process in the repo. Tools:

| Tool | What it does |
| --- | --- |
| `search` | regex search with the query syntax above |
| `find_symbol` | definitions by name, optional `kind` |
| `find_callers` | call sites by name, with the enclosing function |
| `index_status` | whether the repo is indexed and symbols are ready |
| `reindex` | start indexing, re-check changed files, or rebuild |

Results are compact: grouped by file, one `line: text` per hit, capped by
`limit` (default 50) with optional `context`. In a repo with no index,
`search` still answers from a file scan and starts indexing.

Register it with Claude Code, from inside your repo:

```sh
claude mcp add scout -- scout mcp
```

Use `--scope user` to have it in every project, or pass `--root DIR` after
`scout mcp` to pin one repo. For other MCP clients, run `scout mcp` with the
repo as the working directory.

## How it works

- **Index:** immutable, mmap'd segments of file-level trigram posting lists
  (delta varints), with roaring-bitmap tombstones and an atomically swapped
  `manifest.json`. File contents aren't stored; every candidate is verified
  against the working tree with ripgrep's searcher, so a stale index can't
  return wrong lines.
- **Queries:** the regex is turned into a boolean trigram query (Russ Cox's
  codesearch algorithm), evaluated per segment, then verified in parallel.
  Queries with no usable trigrams scan every indexed file at about rg speed.
- **Freshness:** a per-repo daemon (`scout serve`) takes save notices from
  Neovim, polls git's `HEAD` and index for branch switches and pulls, writes
  small delta segments and compacts them in the background. It runs at low
  CPU and I/O priority and exits after 30 idle minutes.
- **Symbols:** a second pass after the text index runs tree-sitter over
  each file and stores definitions and call sites in a sidecar per segment.
  Text search is usable while it runs.

Excluding paths: add a `.scoutignore` at the repo root in gitignore syntax.
A `!pattern` there brings back something `.gitignore` hides, such as a
generated header. `scout status --why PATH` names the rule that skipped a
file.

## Numbers

Linux kernel (97,160 files, 1.2 GB), 4 cores, warm cache, against
`rg --hidden` on the same files:

| Query | scout | rg |
| --- | --- | --- |
| `kmalloc_array` | 14 ms | 349 ms |
| `EXPORT_SYMBOL_GPL\(dma_` | 5 ms | 319 ms |
| `struct\s+file_operations\s+\w+\s*=` | 70 ms | 386 ms |
| `TODO\|FIXME` (10,349 hits) | 39 ms | 342 ms |
| `\bcopy_from_user\b` | 20 ms | 331 ms |
| `a.b` (no trigrams, 332k hits) | 680 ms | 710 ms |
| `calls:kmalloc_array` | 16 ms | |
| `refs:copy_from_user` | 9 ms | |
| `sym:task_struct kind:struct` | 124 ms | |

Results match rg line for line. The text index builds in 8.4 s and takes
145 MB; the symbol pass adds 113 s on 2 background threads (9.5M symbols,
204 MB).

## Development

```sh
cargo test --release --workspace
nvim --headless -u NONE -c "luafile tests/nvim/smoke.lua"
python3 bench/compare.py /path/to/repo target/release/scout
```

## License

MIT
