# Neovim

Needs [snacks.nvim](https://github.com/folke/snacks.nvim) for pickers; without it results go to the quickfix list.

```lua
{
  "Nikhil-Paleti/scout",
  build = "cargo build --release",
  event = "VeryLazy",
  opts = {},
}
```

| Command | Does |
|---|---|
| `:ScoutLive [q]` | live search ([syntax](syntax.md)) |
| `:ScoutDir [q]` | live search in the current file's directory |
| `:ScoutFiles [q]` | find files by name |
| `:ScoutFolders [q]` | find a folder by name, show it in the snacks explorer |
| `:Scout q` | search into the quickfix list |
| `:ScoutWord` | word under the cursor, or the selection |
| `:ScoutSym [name]` | definitions (default: word under the cursor) |
| `:ScoutCallers [name]` | call sites (default: word under the cursor) |
| `:ScoutIndex [rebuild]` | index now, or rebuild |
| `:ScoutStatus` | index state |
| `:checkhealth scout` | setup check |

| LazyVim key | Goes to, in indexed repos |
|---|---|
| `<leader>/`, `<leader>sg` | `:ScoutLive` |
| `<leader>sw` | `:ScoutWord` |
| `<leader><space>`, `<leader>ff` | `:ScoutFiles` |
| `<leader>/` in the snacks explorer | `:ScoutLive` in the highlighted folder |

| New key | Does |
|---|---|
| `<leader>fd` | `:ScoutFolders`, also from inside the explorer; skipped if the key is taken |

A folder pick opens the explorer if it is closed (by pressing your `<leader>e`, so a popup stays a popup), expands the folder and puts the cursor on it.

Before you type, pickers list changed files, recent files, then the current directory.

| Option | Default | Does |
|---|---|---|
| `index_on_start` | `true` | index when Neovim opens a git repo |
| `route_lazyvim_keys` | `true` | take over the keys above |
| `dirs_key` | `"<leader>fd"` | folder search key, `false` for none |
| `explorer_key` | `"<leader>e"` | key that opens your explorer |
| `syntax_hint` | `true` | show the syntax hint in the empty prompt |
| `min_chars` | `3` | live search waits for this many characters |
| `max_results` | `2000` | most results per picker |
| `exclude` | `{}` | gitignore-style globs kept out of the index |
| `notify_on_save` | `true` | reindex files on save |
| `reconcile_on_focus` | `true` | re-check git state on focus |
| `bin` | `nil` | path to the scout binary |
