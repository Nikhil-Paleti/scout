-- Headless test of <leader>/ in the snacks explorer. Needs a snacks.nvim checkout:
--   SNACKS_DIR=/path/to/snacks.nvim nvim --headless -u NONE -c "luafile tests/nvim/explorer.lua"
local here = vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":p:h:h:h")
assert(vim.env.SNACKS_DIR, "set SNACKS_DIR to a snacks.nvim checkout")
vim.opt.rtp:prepend(vim.env.SNACKS_DIR)
vim.opt.rtp:prepend(here)
vim.g.mapleader = " "
vim.cmd("runtime plugin/scout.lua")

local uv = vim.uv or vim.loop
local tmp = vim.fn.tempname()
local function new_repo(path)
  vim.fn.mkdir(path .. "/src/widget", "p")
  vim.fn.writefile({ "int hello_world(void) { return 1; }" }, path .. "/src/a.c")
  vim.fn.writefile({ "void draw_widget(void) {}" }, path .. "/src/widget/draw.c")
  vim.fn.writefile({ "hello_world notes" }, path .. "/README.md")
  vim.fn.system({ "git", "init", "-q", path })
  return uv.fs_realpath(path)
end
local repo = new_repo(tmp .. "/repo")
-- A submodule: its own .git file, but searched through the outer index.
vim.fn.mkdir(repo .. "/vendor/lib/src", "p")
vim.fn.writefile({ "int lib_fn(void);" }, repo .. "/vendor/lib/src/lib.c")
vim.fn.writefile({ "gitdir: ../../.git/modules/vendor/lib" }, repo .. "/vendor/lib/.git")
local bare = new_repo(tmp .. "/bare") -- never indexed
vim.env.SCOUT_CACHE_DIR = tmp .. "/cache"

local failures = 0
local function check(cond, msg)
  if not cond then
    failures = failures + 1
  end
  io.stdout:write((cond and "ok   " or "FAIL ") .. msg .. "\n")
  io.stdout:flush()
end

local Snacks = require("snacks")
Snacks.setup({ picker = { enabled = true }, explorer = { enabled = true } })
require("scout").setup({ index_on_start = false })
local util = require("scout.util")
local daemon = require("scout.daemon")
vim.fn.system({ util.bin(), "build", "--quiet", "--root", repo })
check(vim.v.shell_error == 0, "scout build")
daemon.refresh(repo)
check(vim.wait(5000, function() return daemon.indexed(repo) end, 50), "repo is indexed")
daemon.refresh(bare)
vim.wait(2000, function() return daemon.state(bare) ~= nil end, 50)

local function close_all()
  -- Let finders finish first: closing a picker mid-search trips snacks.
  vim.wait(3000, function()
    for _, p in ipairs(Snacks.picker.get()) do
      if p:is_active() then
        return false
      end
    end
    return true
  end, 20)
  for _, p in ipairs(Snacks.picker.get()) do
    p:close()
  end
  vim.wait(500, function() return #Snacks.picker.get() == 0 end, 20)
end

--- Opens the explorer in `root` with `rel` highlighted; returns the picker
--- and the list buffer's <leader>/ mapping once scout has routed it.
local function explorer_on(root, rel)
  vim.cmd("cd " .. root)
  local p = Snacks.explorer({ cwd = root })
  vim.wait(3000, function() return #p:items() > 0 and not p:is_active() and p.list.win:valid() end, 20)
  -- Moving the cursor is snacks' job; stand in for it with the highlighted item.
  local target = root .. (rel ~= "" and ("/" .. rel) or "")
  p.current = function()
    return { file = target }
  end
  local buf = p.list.win.buf
  local map
  vim.wait(2000, function()
    map = vim.api.nvim_buf_call(buf, function()
      return vim.fn.maparg("<leader>/", "n", false, true)
    end)
    return map.desc == "Search folder (scout when indexed)"
  end, 20)
  return p, map
end

-- A folder in an indexed repo: scout's live search, scoped to that folder.
local p, map = explorer_on(repo, "src/widget")
check(map.desc == "Search folder (scout when indexed)", "explorer's <leader>/ is routed to scout")
map.callback()
--- The first open picker from `source`, once it shows up.
local function opened(source)
  local found
  vim.wait(3000, function()
    found = Snacks.picker.get({ source = source })[1]
    return found ~= nil
  end, 20)
  return found
end
local s = opened("scout")
check(s and s.title == "Scout in src/widget/", "folder opens scout in that folder: " .. tostring(s and s.title))
check(#Snacks.picker.get({ source = "explorer" }) == 1, "explorer stays open under the search")
close_all()

-- A file in a subfolder searches its folder.
p, map = explorer_on(repo, "src/a.c")
map.callback()
s = opened("scout")
check(s and s.title == "Scout in src/", "file searches its folder: " .. tostring(s and s.title))
close_all()

-- A file at the root searches the whole repo.
p, map = explorer_on(repo, "README.md")
map.callback()
s = opened("scout")
check(s and s.title == "Scout", "root file searches the repo: " .. tostring(s and s.title))
close_all()

-- A folder in a submodule uses the outer repo's index.
p, map = explorer_on(repo, "vendor/lib/src")
map.callback()
s = opened("scout")
check(s and s.title == "Scout in vendor/lib/src/", "submodule folder uses the outer index: " .. tostring(s and s.title))
close_all()

-- No index: the explorer's own grep in that folder.
p, map = explorer_on(bare, "src")
check(map.desc == "Search folder (scout when indexed)", "routed in an unindexed repo too")
map.callback()
local g = opened("grep")
check(g and #Snacks.picker.get({ source = "scout" }) == 0, "unindexed repo falls back to snacks grep")
check(g and g:cwd() == bare .. "/src", "grep runs in the folder: " .. tostring(g and g:cwd()))
close_all()

-- Other snacks pickers keep their own <leader>/.
local f = Snacks.picker.files({ cwd = repo })
vim.wait(2000, function() return f.list and f.list.win and f.list.win:valid() end, 20)
vim.wait(300)
local fmap = vim.api.nvim_buf_call(f.list.win.buf, function()
  return vim.fn.maparg("<leader>/", "n", false, true)
end)
check(fmap.desc ~= "Search folder (scout when indexed)", "files picker is left alone")
close_all()

vim.fn.system({ util.bin(), "stop", "--root", repo })
print(failures == 0 and "ALL PASSED" or (failures .. " FAILED"))
vim.cmd((failures == 0 and "qa!" or "cq!"))
