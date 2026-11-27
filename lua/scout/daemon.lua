-- Talks to the per-repo background updater over its Unix socket. Nothing
-- here blocks the editor: sends are fire-and-forget, and a missing socket
-- just drops the message.
local util = require("scout.util")

local M = {}

-- root -> { indexed = bool, socket = string, building = bool, checked = time }
local state = {}

--- Last known state for a root (may be nil before the first check).
function M.state(root)
  return state[root]
end

--- Refreshes the cached status of `root` in the background.
function M.refresh(root, cb)
  local bin = util.bin()
  if not bin then
    return
  end
  util.run({ bin, "status", "--json", "--root", root }, {}, function(code, out)
    if code == 0 then
      local ok, v = pcall(vim.json.decode, out)
      if ok and type(v) == "table" then
        state[root] = {
          indexed = v.indexed == true,
          building = v.building == true,
          socket = v.socket,
          checked = util.uv.now(),
          status = v,
        }
      end
    end
    if cb then
      cb(state[root])
    end
  end)
end

--- True when the root is known to have an index. Kicks off a check the
--- first time a root is seen, so the answer can be false until it returns.
function M.indexed(root)
  local s = state[root]
  if not s then
    M.refresh(root)
    return false
  end
  if not s.indexed and util.uv.now() - s.checked > 5000 then
    M.refresh(root)
  end
  return s.indexed
end

--- Writes protocol lines to the daemon's socket without waiting.
function M.send(root, lines)
  local s = state[root]
  if not (s and s.socket and util.uv.fs_stat(s.socket)) then
    return false
  end
  local pipe = util.uv.new_pipe(false)
  pipe:connect(s.socket, function(err)
    if err then
      pipe:close()
      return
    end
    pipe:write(table.concat(lines, "\n") .. "\n", function()
      pipe:shutdown(function()
        pipe:close()
      end)
    end)
  end)
  return true
end

--- Tells the daemon a file changed. If no daemon runs but an index exists,
--- `scout notify --start` brings one up.
function M.notify(root, path)
  if M.send(root, { "notify " .. path }) then
    return
  end
  local s = state[root]
  local bin = util.bin()
  if s and s.indexed and bin then
    util.run({ bin, "notify", "--start", "--root", root, path }, {}, function()
      M.refresh(root)
    end)
  end
end

function M.reconcile(root)
  M.send(root, { "reconcile" })
end

--- Calls cb once the root has an index, checking every couple of seconds.
function M.when_indexed(root, cb, tries)
  tries = tries or 0
  M.refresh(root, function(st)
    if st and st.indexed then
      cb(st)
    elseif tries < 1800 then
      vim.defer_fn(function()
        M.when_indexed(root, cb, tries + 1)
      end, 2000)
    end
  end)
end

local function serve_cmd(bin, root)
  local cmd = { bin, "serve", "--root", root }
  for _, e in ipairs(require("scout.config").options.exclude or {}) do
    vim.list_extend(cmd, { "--exclude", e })
  end
  return cmd
end

-- Roots started this session, so a :cd back and forth asks only once.
local started = {}

--- Quietly starts the background updater for `root`: it builds the index
--- if there is none and catches up from git if there is. `scout serve`
--- detaches at once and does nothing if one already runs.
function M.start(root)
  local bin = util.bin()
  if not (bin and root) or started[root] then
    return
  end
  started[root] = true
  util.run(serve_cmd(bin, root), {}, function(code)
    if code ~= 0 then
      started[root] = nil
      return
    end
    M.refresh(root)
  end)
end

--- Builds the index in the background (or rebuilds it). The daemon does
--- the work at low priority, so the editor stays responsive.
function M.index(root, rebuild)
  local bin = util.bin()
  if not bin then
    vim.notify("scout: binary not found; build it with `cargo build --release`", vim.log.levels.ERROR)
    return
  end
  local s = state[root]
  if s and s.indexed then
    if rebuild then
      if not M.send(root, { "rebuild" }) then
        local cmd = { bin, "build", "--quiet", "--root", root }
        for _, e in ipairs(require("scout.config").options.exclude or {}) do
          vim.list_extend(cmd, { "--exclude", e })
        end
        util.run(cmd, {}, function(code, _, err)
          if code == 0 then
            vim.notify("scout: index rebuilt for " .. root)
          else
            vim.notify("scout: rebuild failed: " .. vim.trim(err), vim.log.levels.ERROR)
          end
        end)
      end
      vim.notify("scout: rebuilding the index in the background")
      return
    else
      M.send(root, { "reconcile full" })
      vim.notify("scout: re-checking changed files")
      return
    end
  end
  util.run(serve_cmd(bin, root), {}, function(code, _, err)
    if code ~= 0 then
      vim.notify("scout: could not start indexing: " .. vim.trim(err), vim.log.levels.ERROR)
      return
    end
    vim.notify("scout: indexing " .. root .. " in the background")
    M.when_indexed(root, function()
      vim.notify("scout: index ready for " .. root)
    end)
  end)
end

return M
