# scout

Indexed code search for big repos. Regex search in milliseconds from the
terminal, from agents over MCP, or from Neovim. Everything stays local.

## Install

```sh
git clone https://github.com/Nikhil-Paleti/scout
cargo install --path scout/crates/scout
```

## Use

```sh
scout 'TODO|FIXME' file:^src/ lang:cpp   # regex search (rg-style output)
scout sym ParseConfig                    # definitions
scout calls ParseConfig                  # call sites (matched by name only)
scout --json -C 2 retry_count            # JSON lines for tools
```

The first search in a repo starts building the index in the background and
scans files meanwhile. `scout build` builds it in the foreground. `scout
--help` lists every flag and filter.

## Agents

```sh
claude mcp add scout -- scout mcp
```

Tools: `search`, `find_symbol`, `find_callers`, `index_status`, `reindex`.

## Neovim (lazy.nvim)

```lua
{
  "Nikhil-Paleti/scout",
  build = "cargo build --release",
  event = "VeryLazy",
  opts = {},
}
```

`:ScoutLive`, `:Scout`, `:ScoutWord`, `:ScoutSym`, `:ScoutCallers`,
`:ScoutIndex`, `:checkhealth scout`. In indexed repos, LazyVim's
`<leader>/`, `<leader>sg` and `<leader>sw` use scout.

## Speed

Linux kernel (97k files): 5 to 70 ms per query, against 320 to 390 ms for
ripgrep, with identical results.
