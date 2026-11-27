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
scout '(TODO OR FIXME) lang:python'   # search, Google Code Search syntax
scout files trnr                      # find files by name
scout dirs a h bva                    # find a folder: a/b/c/d/e/f/g/h/nvakbva
scout sym Trainer                     # definitions
```

[Examples](docs/examples.md) · [Syntax](docs/syntax.md) · [CLI](docs/cli.md) ·
[Neovim](docs/neovim.md) · [MCP](docs/mcp.md)

## Agents

```sh
claude mcp add scout -- scout mcp
```

## Neovim (lazy.nvim)

```lua
{
  "Nikhil-Paleti/scout",
  build = "cargo build --release",
  event = "VeryLazy",
  opts = {},
}
```

Indexing starts when Neovim opens a git repo. Commands, keys and options:
[docs/neovim.md](docs/neovim.md).

## Speed

Linux kernel (97k files): 5 to 70 ms per query, against 320 to 390 ms for
ripgrep, with identical results.
