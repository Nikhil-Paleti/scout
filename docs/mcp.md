# MCP

```sh
claude mcp add scout -- scout mcp
```

| Tool | Arguments | Returns |
|---|---|---|
| `search` | `query`, `limit`, `context` | file-name hits, then matching lines ([syntax](syntax.md)) |
| `find_files` | `query`, `limit` | paths by fuzzy name or path regex |
| `find_dirs` | `query`, `limit` | folders, matched the same way |
| `find_symbol` | `name`, `kind`, `filters`, `limit`, `context` | definitions |
| `find_callers` | `name`, `filters`, `limit`, `context` | call sites, with the enclosing function |
| `index_status` | none | files, symbols, updater state |
| `reindex` | `full` | catch up, or rebuild with `full: true` |

`filters` takes query filters, e.g. `file:^src/ lang:cpp -file:_test`. With no index yet, `search`, `find_files` and `find_dirs` still answer and indexing starts in the background.
