-- What the pickers list before you type: changed files first, then files
-- you opened recently, then the files next to the current one. It never
-- queries the index. git runs in the background; its answer is cached per
-- root and the picker refreshes when it arrives.
local util = require("scout.util")

local M = {}

M.hint = [[sym:  file:  lang:  "exact"  -term]]

local FRESH_MS = 5000
local MAX_CHANGED = 2000
local MAX_NEARBY = 300

-- root -> { files = { {rel, tag} }, at = ms, running = bool, waiters = {fn} }
local cache = {}

-- Undoes git's C-style quoting of unusual paths.
local function unquote(p)
  if p:sub(1, 1) ~= '"' or p:sub(-1) ~= '"' then
    return p
  end
  p = p:sub(2, -2)
  local esc = { n = "\n", t = "\t" }
  local out, i = {}, 1
  while i <= #p do
    local c = p:sub(i, i)
    if c == "\\" then
      local oct = p:match("^[0-7][0-7][0-7]", i + 1)
      if oct then
        out[#out + 1] = string.char(tonumber(oct, 8))
        i = i + 4
      else
        local n = p:sub(i + 1, i + 1)
        out[#out + 1] = esc[n] or n
        i = i + 2
      end
    else
      out[#out + 1] = c
      i = i + 1
    end
  end
  return table.concat(out)
end

-- `git status --porcelain` lines (not -z: older Neovim turns NUL into
-- newlines in job output).
local function parse_status(out)
  local files = {}
  for line in out:gmatch("[^\n]+") do
    if #files >= MAX_CHANGED then
      break
    end
    local xy, path = line:sub(1, 2), line:sub(4)
    if xy:find("[RC]") then
      path = path:match(" %-> (.*)$") or path
    end
    path = unquote(path)
    if not xy:find("D") and path ~= "" and path:sub(-1) ~= "/" then
      files[#files + 1] = { path, xy == "??" and "new" or "changed" }
    end
  end
  return files
end

--- Changed and untracked files under `root`, newest first, from the cache.
--- Starts a refresh when the cache is missing or stale; `on_update` runs
--- once that refresh returns with a different list.
function M.changed(root, on_update)
  local c = cache[root]
  local now = util.uv.now()
  if not c or (now - c.at > FRESH_MS and not c.running) then
    c = c or { files = nil, at = 0, waiters = {} }
    cache[root] = c
    if not c.running then
      c.running = true
      local before = c.files and vim.inspect(c.files)
      util.run({ "git", "-C", root, "-c", "core.quotePath=false", "status", "--porcelain=v1", "--untracked-files=all" }, {}, function(code, out)
        c.running = false
        c.at = util.uv.now()
        local files = code == 0 and parse_status(out) or {}
        for _, f in ipairs(files) do
          local st = util.uv.fs_stat(root .. "/" .. f[1])
          f[3] = st and st.mtime.sec or 0
        end
        table.sort(files, function(a, b)
          return a[3] > b[3]
        end)
        c.files = files
        local waiters = c.waiters
        c.waiters = {}
        if vim.inspect(files) ~= before then
          for _, w in ipairs(waiters) do
            w()
          end
        end
      end)
    end
  end
  if c.running and on_update then
    table.insert(c.waiters, on_update)
  end
  return c.files or {}
end

--- Files opened recently under `root`: listed buffers by last use, then
--- v:oldfiles. Paths are relative to `root`.
function M.recent(root)
  local out, seen = {}, {}
  local prefix = root .. "/"
  local function add(abs)
    if abs == "" or seen[abs] then
      return
    end
    seen[abs] = true
    abs = util.uv.fs_realpath(abs) or abs
    if abs:sub(1, #prefix) == prefix then
      local st = util.uv.fs_stat(abs)
      if st and st.type == "file" then
        out[#out + 1] = abs:sub(#prefix + 1)
      end
    end
  end
  local bufs = vim.fn.getbufinfo({ buflisted = 1 })
  table.sort(bufs, function(a, b)
    return (a.lastused or 0) > (b.lastused or 0)
  end)
  for _, b in ipairs(bufs) do
    add(b.name)
  end
  for _, f in ipairs(vim.v.oldfiles or {}) do
    add(f)
  end
  return out
end

--- Files directly inside `dir` (relative to `root`; nil for the root).
function M.nearby(root, dir)
  local out = {}
  local abs = dir and (root .. "/" .. dir) or root
  local ok, iter = pcall(vim.fs.dir, abs)
  if not ok then
    return out
  end
  for name, type in iter do
    if type == "file" then
      out[#out + 1] = dir and (dir .. "/" .. name) or name
    end
  end
  table.sort(out)
  if #out > MAX_NEARBY then
    out = vim.list_slice(out, 1, MAX_NEARBY)
  end
  return out
end

--- The list to show for an empty query, as { path, tag } pairs.
--- opts.dir limits it to a directory; opts.current (relative path) is left
--- out and gives the default directory; opts.on_update as in M.changed.
function M.list(root, opts)
  opts = opts or {}
  local dir = opts.dir
  if not dir and opts.current then
    local d = vim.fn.fnamemodify(opts.current, ":h")
    dir = d ~= "." and d or nil
  end
  local scope = opts.dir and (opts.dir .. "/") or nil
  local out, seen = {}, {}
  local function add(path, tag)
    if seen[path] or path == opts.current then
      return
    end
    if scope and path:sub(1, #scope) ~= scope then
      return
    end
    seen[path] = true
    out[#out + 1] = { path, tag }
  end
  for _, f in ipairs(M.changed(root, opts.on_update)) do
    add(f[1], f[2])
  end
  for _, p in ipairs(M.recent(root)) do
    add(p, "recent")
  end
  for _, p in ipairs(M.nearby(root, dir)) do
    add(p, nil)
  end
  return out
end

--- Forgets cached git state (tests).
function M.reset()
  cache = {}
end

return M
