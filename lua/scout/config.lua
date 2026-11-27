local M = {}

M.defaults = {
  -- Path to the scout binary. nil: the plugin's own target/release/scout,
  -- then `scout` on $PATH.
  bin = nil,
  -- Send LazyVim's grep keys (<leader>/, <leader>sg, <leader>sw) to scout
  -- when the project has an index. Without an index they stay on rg. Also
  -- <leader>/ in the snacks explorer: it searches the highlighted folder.
  route_lazyvim_keys = true,
  -- Find folders by name (:ScoutFolders) and open the pick in the snacks
  -- explorer. Mapped only when nothing else uses the key; false to skip.
  dirs_key = "<leader>fd",
  -- The key that opens your explorer (LazyVim: <leader>e). When no explorer
  -- is open, a folder pick presses it first, so the explorer opens the way
  -- you set it up (sidebar or popup), then shows the folder.
  explorer_key = "<leader>e",
  -- Show `sym:  file:  lang:  "exact"  -term` in the empty search prompt.
  syntax_hint = true,
  -- Live search waits for this many characters.
  min_chars = 3,
  -- Most results a picker asks for.
  max_results = 2000,
  -- Glob patterns to keep out of the index on this machine, in gitignore
  -- syntax. Used by :ScoutIndex.
  exclude = {},
  -- Tell the background updater about saves, and re-check git state when
  -- Neovim regains focus.
  notify_on_save = true,
  reconcile_on_focus = true,
  -- Start the background indexer when Neovim opens a git repo, and on :cd
  -- into another one: it builds the index if there is none, else catches up
  -- with what changed. Runs detached at low priority; skips $HOME and
  -- folders outside git.
  index_on_start = true,
}

M.options = vim.deepcopy(M.defaults)

function M.setup(opts)
  M.options = vim.tbl_deep_extend("force", vim.deepcopy(M.defaults), opts or {})
  return M.options
end

return M
