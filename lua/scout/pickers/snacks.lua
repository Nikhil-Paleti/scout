-- snacks.picker sources for scout: live search, a fixed query, symbols
-- and callers.
local search = require("scout.search")
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

local function finder(root, get_query, flags)
  return function(opts, ctx)
    local cfg = require("scout.config").options
    local query = get_query(ctx)
    if not query or query == "" or (opts.live and #query < cfg.min_chars) then
      return function() end
    end
    flags = flags or {}
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

local function near(root)
  local name = vim.api.nvim_buf_get_name(0)
  if name ~= "" and name:sub(1, #root + 1) == root .. "/" then
    return name:sub(#root + 2)
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
    finder = finder(root, function(ctx)
      return ctx.filter.search
    end, { dir = opts.dir }),
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
  return require("snacks").picker.pick({
    source = "scout_files",
    title = "Scout files",
    format = "file",
    pattern = opts.search,
    finder = function(popts, ctx)
      local proc = {
        cmd = cmd[1],
        args = vim.list_slice(cmd, 2),
        cwd = root,
        notify = false,
        transform = function(item)
          item.cwd = root
          item.file = item.text
        end,
      }
      return require("snacks.picker.source.proc").proc(proc_opts(popts, ctx, proc), ctx)
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
