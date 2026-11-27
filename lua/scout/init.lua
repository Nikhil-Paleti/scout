-- scout: indexed code search for Neovim. The editor never indexes or
-- scans: each search is a `scout` child process, and a per-repo
-- background updater keeps the index fresh.
local config = require("scout.config")
local daemon = require("scout.daemon")
local search = require("scout.search")
local util = require("scout.util")

local M = {}

local function snacks()
  local p = require("scout.pickers.snacks")
  return p.available() and p or nil
end

--- Live picker (snacks), or a prompt that fills the quickfix list.
function M.live(opts)
  opts = opts or {}
  local p = snacks()
  if p then
    return p.live(opts)
  end
  vim.ui.input({ prompt = "Scout: ", default = opts.search }, function(q)
    if q and q ~= "" then
      search.quickfix(q, opts)
    end
  end)
end

--- Results of one query: a picker if snacks is there, else quickfix.
function M.pick(query, opts)
  opts = opts or {}
  local p = snacks()
  if p then
    return p.query(query, opts)
  end
  search.quickfix(query, opts)
end

--- The word under the cursor (or the selection), as a literal.
function M.word()
  local w, whole = util.word_or_selection()
  if w == "" then
    return
  end
  M.pick(w, { fixed = true, word = whole, contents_only = true, title = "Scout " .. w })
end

--- Directory of the current buffer, relative to the root (Google Code
--- Search's `.`: search in the current directory).
local function buf_dir(root)
  local name = vim.api.nvim_buf_get_name(0)
  if name == "" or name:sub(1, #root + 1) ~= root .. "/" then
    return nil
  end
  local rel = vim.fn.fnamemodify(name:sub(#root + 2), ":h")
  return rel ~= "." and rel or nil
end

--- Live search limited to the current file's directory.
function M.live_dir(opts)
  opts = opts or {}
  local root = util.root()
  local dir = opts.dir or buf_dir(root)
  local p = snacks()
  if p then
    return p.live({ dir = dir, search = opts.search })
  end
  vim.ui.input({ prompt = "Scout in " .. (dir or ".") .. "/: " }, function(q)
    if q and q ~= "" then
      search.quickfix(search.dir_filter(dir) .. q, {})
    end
  end)
end

--- Find files by name: a snacks picker over every indexed path, or a
--- prompt for `scout files QUERY` and a list to pick from.
function M.files(opts)
  opts = opts or {}
  local p = snacks()
  if p then
    return p.files(opts)
  end
  local root = util.root()
  local function run(q)
    local cmd = search.files_cmd(q, { root = root })
    if not cmd then
      vim.notify("scout: binary not found", vim.log.levels.ERROR)
      return
    end
    util.run(cmd, {}, function(code, out, err)
      if code == 2 then
        vim.notify("scout: " .. vim.trim(err), vim.log.levels.ERROR)
        return
      end
      local paths = vim.split(out, "\n", { trimempty = true })
      if #paths == 0 then
        vim.notify("scout: no files match " .. q)
        return
      end
      vim.ui.select(paths, { prompt = "Scout files" }, function(choice)
        if choice then
          vim.cmd.edit(vim.fn.fnameescape(root .. "/" .. choice))
        end
      end)
    end)
  end
  if opts.search and opts.search ~= "" then
    return run(opts.search)
  end
  vim.ui.input({ prompt = "Scout files: " }, function(q)
    if q and q ~= "" then
      run(q)
    end
  end)
end

local function name_arg(arg)
  if arg and arg ~= "" then
    return arg
  end
  return vim.fn.expand("<cword>")
end

--- Definitions of a name (default: word under the cursor).
function M.sym(name)
  name = name_arg(name)
  if name ~= "" then
    M.pick("sym:" .. name, { title = "Definitions of " .. name })
  end
end

--- Call sites of a name (default: word under the cursor). Matched by name,
--- so same-named functions mix.
function M.callers(name)
  name = name_arg(name)
  if name ~= "" then
    M.pick("calls:" .. name, { title = "Callers of " .. name })
  end
end

function M.status()
  local bin = util.bin()
  if not bin then
    vim.notify("scout: binary not found", vim.log.levels.ERROR)
    return
  end
  util.run({ bin, "status", "--root", util.root() }, {}, function(_, out, err)
    vim.notify(vim.trim(out ~= "" and out or err), vim.log.levels.INFO, { title = "scout" })
  end)
end

local function route_keys()
  local routes = {
    { "<leader>/", "n", function() M.live() end },
    { "<leader>sg", "n", function() M.live() end },
    { "<leader>sw", "n", M.word },
    { "<leader>sw", "x", M.word },
    -- LazyVim's find files (root dir).
    { "<leader><space>", "n", function() M.files() end },
    { "<leader>ff", "n", function() M.files() end },
  }
  for _, r in ipairs(routes) do
    local lhs, mode, fn = r[1], r[2], r[3]
    local orig = vim.fn.maparg(lhs, mode, false, true)
    -- Only take over keys something already uses for grep (LazyVim).
    if orig and orig.lhs then
      vim.keymap.set(mode, lhs, function()
        if daemon.indexed(util.root()) then
          return fn()
        end
        if orig.callback then
          return orig.callback()
        end
        local keys = vim.api.nvim_replace_termcodes(orig.rhs or "", true, true, true)
        vim.api.nvim_feedkeys(keys, orig.noremap == 1 and "n" or "m", false)
      end, { desc = (orig.desc or "Search") .. " (scout when indexed)", silent = true })
    end
  end
end

--- `dir` relative to `root`: "" for the root itself, nil when outside it.
local function rel_to(root, dir)
  dir = util.uv.fs_realpath(dir) or dir
  if dir == root then
    return ""
  end
  if dir:sub(1, #root + 1) == root .. "/" then
    return dir:sub(#root + 2)
  end
end

--- The nearest indexed git root holding `dir`. A submodule or nested repo
--- has its own .git but is searched through the outer repo's index.
local function indexed_root(dir)
  local root = util.git_root(dir)
  while root and not daemon.indexed(root) do
    local parent = vim.fs.dirname(root)
    root = parent ~= root and util.git_root(parent) or nil
  end
  return root
end

--- The snacks explorer whose list window shows `buf`.
local function explorer_for(buf)
  for _, p in ipairs(require("snacks").picker.get({ source = "explorer" })) do
    if p.list and p.list.win and p.list.win.buf == buf then
      return p
    end
  end
end

--- In the snacks explorer, <leader>/ greps the highlighted folder (or the
--- highlighted file's folder). In an indexed repo it runs scout's live search
--- in that folder instead; elsewhere the explorer's own mapping runs.
local function route_explorer_buf(buf)
  if not vim.api.nvim_buf_is_valid(buf) or not explorer_for(buf) then
    return
  end
  local lhs = "<leader>/"
  local orig = vim.api.nvim_buf_call(buf, function()
    return vim.fn.maparg(lhs, "n", false, true)
  end)
  if not (orig and orig.lhs and orig.buffer == 1) or orig.desc == "Search folder (scout when indexed)" then
    return
  end
  vim.keymap.set("n", lhs, function()
    local p = explorer_for(buf)
    local item = p and p:current()
    local dir = item and require("snacks.picker.util").dir(item)
    local root = dir and indexed_root(dir)
    local rel = root and rel_to(root, dir)
    if rel then
      return M.live({ root = root, dir = rel ~= "" and rel or nil })
    end
    if orig.callback then
      return orig.callback()
    end
    local keys = vim.api.nvim_replace_termcodes(orig.rhs or "", true, true, true)
    vim.api.nvim_feedkeys(keys, orig.noremap == 1 and "n" or "m", false)
  end, { buffer = buf, desc = "Search folder (scout when indexed)", silent = true, nowait = true })
end

local function route_explorer()
  vim.api.nvim_create_autocmd("FileType", {
    group = vim.api.nvim_create_augroup("scout_explorer", { clear = true }),
    pattern = "snacks_picker_list",
    callback = function(ev)
      -- After snacks has set the list window's own keys.
      vim.schedule(function()
        route_explorer_buf(ev.buf)
      end)
    end,
  })
end

--- Starts the indexer for the current file's repo, else cwd's.
local function index_here()
  local name = vim.api.nvim_buf_get_name(0)
  daemon.start(util.git_root(name ~= "" and vim.fs.dirname(name) or nil))
end

local function autocmds()
  local g = vim.api.nvim_create_augroup("scout", { clear = true })
  local cfg = config.options
  if cfg.notify_on_save then
    vim.api.nvim_create_autocmd("BufWritePost", {
      group = g,
      callback = function(ev)
        local file = vim.api.nvim_buf_get_name(ev.buf)
        if file == "" then
          return
        end
        local root = util.root(ev.buf)
        if daemon.indexed(root) then
          daemon.notify(root, util.uv.fs_realpath(file) or file)
        end
      end,
    })
  end
  if cfg.reconcile_on_focus then
    vim.api.nvim_create_autocmd("FocusGained", {
      group = g,
      callback = function()
        local root = util.root()
        if daemon.indexed(root) then
          daemon.reconcile(root)
        end
      end,
    })
  end
  if cfg.index_on_start then
    vim.api.nvim_create_autocmd("VimEnter", { group = g, callback = index_here })
    vim.api.nvim_create_autocmd("DirChanged", {
      group = g,
      callback = function()
        daemon.start(util.git_root())
      end,
    })
  end
  -- Learn whether a project is indexed before the first keypress.
  vim.api.nvim_create_autocmd({ "BufEnter", "DirChanged" }, {
    group = g,
    callback = function()
      daemon.indexed(util.root())
    end,
  })
end

function M.setup(opts)
  config.setup(opts)
  autocmds()
  if config.options.route_lazyvim_keys then
    route_keys()
    route_explorer()
  end
  daemon.indexed(util.root())
  -- Loaded after startup (lazy.nvim's VeryLazy): VimEnter has passed.
  if config.options.index_on_start and vim.v.vim_did_enter == 1 then
    vim.schedule(index_here)
  end
end

return M
