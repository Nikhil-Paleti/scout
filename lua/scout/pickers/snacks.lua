-- snacks.picker sources for scout: live search, a fixed query, symbols
-- and callers.
local search = require("scout.search")
local start = require("scout.start")
local util = require("scout.util")

local M = {}

function M.available()
  return pcall(require, "snacks.picker")
end

-- Whether this snacks has ctx:opts() (newer releases). Walks metatables
-- with rawget, since older releases warn on any unknown key.
local function has_ctx_opts(ctx)
  local t = ctx
  for _ = 1, 10 do
    local mt = getmetatable(t)
    local idx = mt and mt.__index
    if type(idx) ~= "table" then
      return false
    end
    if type(rawget(idx, "opts")) == "function" then
      return true
    end
    t = idx
  end
  return false
end

--- Process config for snacks' proc source, old and new API.
local function proc_opts(opts, ctx, proc)
  if has_ctx_opts(ctx) then
    return ctx:opts(proc)
  end
  return { opts, proc }
end

--- The current file relative to `root`, if it is inside it.
local function near(root)
  local name = vim.api.nvim_buf_get_name(0)
  if name ~= "" and name:sub(1, #root + 1) == root .. "/" then
    return name:sub(#root + 2)
  end
end

local function start_item(root, path, tag)
  return { file = path, text = path, cwd = root, line = tag and ("[" .. tag .. "]") or nil }
end

--- Re-runs the picker's finder once git answers, if nothing was typed.
local function refresher(picker)
  return function()
    if picker and not picker.closed and picker.input and picker.input:get() == "" then
      picker:find()
    end
  end
end

--- Shows the syntax hint in the empty prompt.
local hint_ns = vim.api.nvim_create_namespace("scout.hint")
local function show_hint(picker)
  if not require("scout.config").options.syntax_hint then
    return
  end
  local buf = picker.input and picker.input.win and picker.input.win.buf
  if not (buf and vim.api.nvim_buf_is_valid(buf)) then
    return
  end
  local function update()
    if not vim.api.nvim_buf_is_valid(buf) then
      return true
    end
    vim.api.nvim_buf_clear_namespace(buf, hint_ns, 0, -1)
    if (vim.api.nvim_buf_get_lines(buf, 0, 1, false)[1] or "") == "" then
      vim.api.nvim_buf_set_extmark(buf, hint_ns, 0, 0, {
        virt_text = { { start.hint, "Comment" } },
        virt_text_pos = "eol",
      })
    end
  end
  update()
  vim.api.nvim_create_autocmd({ "TextChanged", "TextChangedI" }, { buffer = buf, callback = update })
end

local function finder(root, get_query, flags)
  return function(opts, ctx)
    local cfg = require("scout.config").options
    local query = get_query(ctx)
    flags = flags or {}
    if flags.start and (not query or query == "") then
      -- Nothing typed yet: changed, recent and nearby files.
      local list = start.list(root, { dir = flags.dir, current = flags.current, on_update = refresher(ctx.picker) })
      return function(cb)
        for _, f in ipairs(list) do
          cb(start_item(root, f[1], f[2]))
        end
      end
    end
    if not query or query == "" or (opts.live and #query < cfg.min_chars) then
      return function() end
    end
    if flags.dir then
      query = search.dir_filter(flags.dir) .. query
    end
    local cmd = search.cmd(query, {
      root = root,
      fixed = flags.fixed,
      word = flags.word,
      contents_only = flags.contents_only,
      near = opts.near,
    })
    if not cmd then
      return function() end
    end
    local proc = {
      cmd = cmd[1],
      args = vim.list_slice(cmd, 2),
      cwd = root,
      notify = false,
      transform = function(item)
        local h = search.decode(item.text)
        if not h then
          return false
        end
        item.cwd = root
        item.file = h.path
        if h.kind == "file" then
          -- A file-name hit: open the file, no line.
          item.text = h.path
          item.line = "[file]"
          return
        end
        item.pos = { h.line, h.col - 1 }
        local note = search.note(h)
        item.line = note and (h.text .. "  " .. note) or h.text
        item.text = h.path .. ":" .. h.line .. ":" .. h.text
        -- Match highlights: 1-based byte positions within the line.
        local positions = {}
        for _, r in ipairs(h.ranges or {}) do
          for i = r[1] + 1, r[2] do
            positions[#positions + 1] = i
          end
        end
        if #positions > 0 then
          item.positions = positions
          item.end_pos = { h.line, h.ranges[1][2] }
        end
      end,
    }
    return require("snacks.picker.source.proc").proc(proc_opts(opts, ctx, proc), ctx)
  end
end

--- Live search: results update as you type. opts.search pre-fills it;
--- opts.dir limits it to a directory (relative to the root).
function M.live(opts)
  opts = opts or {}
  local root = opts.root or util.root()
  return require("snacks").picker.pick({
    source = "scout",
    title = opts.dir and ("Scout in " .. opts.dir .. "/") or "Scout",
    live = true,
    supports_live = true,
    search = opts.search,
    near = near(root),
    format = "file",
    show_empty = true,
    on_show = show_hint,
    finder = finder(root, function(ctx)
      return ctx.filter.search
    end, { dir = opts.dir, start = true, current = near(root) }),
  })
end

--- File names: every indexed path from `scout files`, fuzzy matched by
--- snacks as you type.
function M.files(opts)
  opts = opts or {}
  local root = opts.root or util.root()
  local cmd = search.files_cmd(nil, { root = root })
  if not cmd then
    return
  end
  local current = near(root)
  return require("snacks").picker.pick({
    source = "scout_files",
    title = "Scout files",
    format = "file",
    pattern = opts.search,
    finder = function(popts, ctx)
      local list = start.list(root, { current = current, on_update = refresher(ctx.picker) })
      local seen = {}
      for _, f in ipairs(list) do
        seen[f[1]] = true
      end
      local proc = {
        cmd = cmd[1],
        args = vim.list_slice(cmd, 2),
        cwd = root,
        notify = false,
        transform = function(item)
          if seen[item.text] then
            return false
          end
          item.cwd = root
          item.file = item.text
        end,
      }
      local all = require("snacks.picker.source.proc").proc(proc_opts(popts, ctx, proc), ctx)
      -- Changed, recent and nearby files first: with nothing typed snacks
      -- keeps this order; typing sorts by match score.
      return function(cb)
        for _, f in ipairs(list) do
          cb(start_item(root, f[1], f[2]))
        end
        all(cb)
      end
    end,
  })
end

--- Folders that hold the changed, recent and nearby files, nearest first.
local function start_dirs(root, current)
  local seen, out = {}, {}
  for _, f in ipairs(start.list(root, { current = current })) do
    local d = vim.fs.dirname(f[1])
    if d and d ~= "." and d ~= "" and not seen[d] then
      seen[d] = true
      out[#out + 1] = d
    end
  end
  return out, seen
end

--- Folders: every folder holding an indexed file, from `scout dirs`,
--- fuzzy matched by snacks as you type (`a h bva` finds
--- a/b/c/d/e/f/g/h/nvakbva). Picking one calls opts.confirm(abs_path).
function M.dirs(opts)
  opts = opts or {}
  local root = opts.root or util.root()
  local cmd = search.files_cmd(nil, { root = root, dirs = true })
  if not cmd then
    return
  end
  local current = near(root)
  return require("snacks").picker.pick({
    source = "scout_dirs",
    title = "Scout folders",
    format = "file",
    pattern = opts.search,
    finder = function(popts, ctx)
      local first, seen = start_dirs(root, current)
      local function item(path)
        return { file = path, text = path, dir = true, cwd = root }
      end
      local proc = {
        cmd = cmd[1],
        args = vim.list_slice(cmd, 2),
        cwd = root,
        notify = false,
        transform = function(it)
          if seen[it.text] then
            return false
          end
          it.cwd = root
          it.file = it.text
          it.dir = true
        end,
      }
      local all = require("snacks.picker.source.proc").proc(proc_opts(popts, ctx, proc), ctx)
      return function(cb)
        for _, d in ipairs(first) do
          cb(item(d))
        end
        all(cb)
      end
    end,
    confirm = function(picker, it)
      picker:close()
      if it and opts.confirm then
        vim.schedule(function()
          opts.confirm(root .. "/" .. it.file)
        end)
      end
    end,
  })
end

--- One query, results filtered by fuzzy matching as you type.
function M.query(query, opts)
  opts = opts or {}
  local root = opts.root or util.root()
  return require("snacks").picker.pick({
    source = "scout",
    title = opts.title or ("Scout " .. query),
    near = near(root),
    format = "file",
    finder = finder(root, function()
      return query
    end, opts),
  })
end

return M
