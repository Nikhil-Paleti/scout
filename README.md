# scout

Indexed code search for big repos. Regex search in milliseconds from the
terminal, from agents over MCP, or from Neovim. Everything stays local.

## Install

```sh
git clone https://github.com/Nikhil-Paleti/scout
cargo install --path scout/crates/scout
```

## Use

Queries use Google Code Search syntax: terms are regexes, case insensitive,
and all of them must match in the file. File names match too, and come first.

```sh
scout '(TODO OR FIXME)' file:^src/ lang:cpp # regex search (rg-style output)
scout 'file:_test\.go$'                     # list files
scout files wdgt draw                       # fuzzy file names
scout sym ParseConfig                       # definitions (also function:, class:)
scout calls ParseConfig                     # call sites (matched by name only)
scout --json -C 2 retry_count               # JSON lines for tools
```

The first search in a repo starts building the index in the background and
scans files meanwhile. `scout build` builds it in the foreground. `scout
--help` lists every operator, filter and flag.

## Agents

```sh
claude mcp add scout -- scout mcp
```

Tools: `search`, `find_files`, `find_symbol`, `find_callers`, `index_status`,
`reindex`.

## Neovim (lazy.nvim)

```lua
{
  "Nikhil-Paleti/scout",
  build = "cargo build --release",
  event = "VeryLazy",
  opts = {},
}
```

`:ScoutLive`, `:ScoutDir` (this file's directory), `:ScoutFiles`, `:Scout`,
`:ScoutWord`, `:ScoutSym`, `:ScoutCallers`, `:ScoutIndex`, `:checkhealth scout`.
In indexed repos, LazyVim's `<leader>/`, `<leader>sg`, `<leader>sw`,
`<leader><space>` and `<leader>ff` use scout.

## Speed

Linux kernel (97k files): 5 to 70 ms per query, against 320 to 390 ms for
ripgrep, with identical results.
