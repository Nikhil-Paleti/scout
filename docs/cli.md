# CLI

Plain terms need no quotes: `scout calls launch -file:test`. Quote anything the shell would change, like `"exact"` phrases or `|`. A `-term` is an exclusion unless it is a flag below; put `--` before a query that starts with one.

| Command | Does |
|---|---|
| `scout QUERY` | search ([syntax](syntax.md)) |
| `scout files [QUERY]` | file names: fuzzy words, or a path regex; `-term` drops matching paths |
| `scout sym NAME [FILTERS]` | definitions, same as `scout 'sym:NAME'` |
| `scout calls NAME [FILTERS]` | call sites, same as `scout 'calls:NAME'` |
| `scout refs NAME [FILTERS]` | whole-word uses |
| `scout build` | build the index in the foreground |
| `scout status [--json]` | index and updater state |
| `scout serve` | start the background updater |
| `scout stop` | stop it |
| `scout mcp` | MCP server on stdio |

| Search flag | Does |
|---|---|
| `--json` | one JSON object per hit, then a summary |
| `--vimgrep` | `path:line:col:text`, contents only |
| `-l` | matching file names only |
| `-C N`, `-A N`, `-B N` | context lines |
| `-m N` | at most N hits (default 5000) |
| `-F`, `-w` | fixed string, whole word |
| `-s`, `-i`, `-S` | case sensitive, insensitive (default), smart |
| `--no-file-hits` | contents only, no file-name hits |
| `--near PATH` | rank results near this file first |
| `--root DIR` | repo root (default: git root of the current directory) |

| `--json` hit field | Value |
|---|---|
| `type` | `hit` or `summary` |
| `path`, `line`, `col` | location (1-based) |
| `text` | the line |
| `ranges` | byte ranges of the match |
| `kind` | `text`, `file`, or the symbol kind (`function`, `call`, ...) |
| `symbol`, `in` | symbol name; enclosing function of a call |
| `before`, `after` | context lines, with `-C` |

The summary has `matches`, `files`, `file_hits`, `truncated`, `elapsed_ms` and `index_status` (`ready`, or `building` while the first index builds).

| Exit code | Means |
|---|---|
| 0 | hits |
| 1 | no hits |
| 2 | error |
