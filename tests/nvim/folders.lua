-- Headless test of folder search (<leader>fd, :ScoutFolders) and showing
-- the pick in the snacks explorer. Needs a snacks.nvim checkout:
--   SNACKS_DIR=/path/to/snacks.nvim nvim --headless -u NONE -c "luafile tests/nvim/folders.lua"
local here = vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":p:h:h:h")
assert(vim.env.SNACKS_DIR, "set SNACKS_DIR to a snacks.nvim checkout")
vim.opt.rtp:prepend(vim.env.SNACKS_DIR)
vim.opt.rtp:prepend(here)
vim.g.mapleader = " "
vim.cmd("runtime plugin/scout.lua")

local uv = vim.uv or vim.loop
local tmp = vim.fn.tempname()
local repo = tmp .. "/repo"
local deep = "a/b/c/d/e/f/g/h/nvakbva"
for _, f in ipairs({ deep .. "/main.c", deep .. "/sub/util.c", "a/b/c/d/e/f/g/h/other/x.c", "src/widget/draw.c", "README.md" }) do
  vim.fn.mkdir(vim.fs.dirname(repo .. "/" .. f), "p")
  vim.fn.writefile({ "int x;" }, repo .. "/" .. f)
end
vim.fn.system({ "git", "init", "-q", repo })
repo = uv.fs_realpath(repo)
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
-- The user's own explorer key: a popup that closes when focus leaves.
local popups = 0
vim.keymap.set("n", "<leader>e", function()
  popups = popups + 1
  Snacks.explorer({ cwd = repo, auto_close = true, layout = { preset = "default", preview = false } })
end, { desc = "Explorer popup" })

require("scout").setup({ index_on_start = false })
local util = require("scout.util")
local daemon = require("scout.daemon")
vim.cmd("cd " .. repo)
vim.fn.system({ util.bin(), "build", "--quiet", "--root", repo })
check(vim.v.shell_error == 0, "scout build")
daemon.refresh(repo)
check(vim.wait(5000, function() return daemon.indexed(repo) end, 50), "repo is indexed")

local map = vim.fn.maparg("<leader>fd", "n", false, true)
check(map.desc == "Find folder (scout)", "<leader>fd is mapped")
check(vim.fn.exists(":ScoutFolders") == 2, ":ScoutFolders exists")

local function idle()
  vim.wait(3000, function()
    for _, p in ipairs(Snacks.picker.get()) do
      if p:is_active() then
        return false
      end
    end
    return true
  end, 20)
end

local function close_all()
  idle()
  for _, p in ipairs(Snacks.picker.get()) do
    p:close()
  end
  vim.wait(500, function() return #Snacks.picker.get() == 0 end, 20)
end

local function opened(source)
  local found
  vim.wait(3000, function()
    found = Snacks.picker.get({ source = source })[1]
    return found ~= nil
  end, 20)
  return found
end

--- Types `q` into the folder picker; returns the paths it shows, best first.
local function pick_list(p, q)
  vim.wait(3000, function() return p.input.win:valid() end, 20)
  idle()
  p.input:set(q, "")
  p:find({ refresh = false }) -- headless: no TextChanged, so match by hand
  vim.wait(100)
  idle()
  local out = {}
  for _, it in ipairs(p:items()) do
    out[#out + 1] = it.file
  end
  return out
end

--- Waits until the explorer's cursor is on `abs`; returns the explorer.
local function revealed(abs)
  local e, cur
  vim.wait(4000, function()
    e = Snacks.picker.get({ source = "explorer" })[1]
    cur = e and not e:is_active() and e:current()
    return cur and cur.file == abs
  end, 20)
  return e, cur
end

-- 1. No explorer open: <leader>fd, "a h bva", Enter.
map.callback()
local s = opened("scout_dirs")
check(s ~= nil, "<leader>fd opens the folder picker")
local list = pick_list(s, "a h bva")
check(list[1] == deep, "\"a h bva\" puts the deep folder first: " .. tostring(list[1]))
check(not vim.tbl_contains(list, "a/b/c/d/e/f/g/h/other"), "folders without bva are left out")
s:action("confirm")
local e, cur = revealed(repo .. "/" .. deep)
check(popups == 1, "the user's own explorer key opened the explorer")
check(cur and cur.file == repo .. "/" .. deep, "explorer cursor is on the folder: " .. tostring(cur and cur.file))
check(cur and cur.open == true, "the folder is expanded")
local shows_main = false
for _, it in ipairs(e and e:items() or {}) do
  shows_main = shows_main or it.file == repo .. "/" .. deep .. "/main.c"
end
check(shows_main, "its files are listed")
local win = vim.api.nvim_get_current_win()
check(e and e.list.win.win == win, "focus is in the explorer")

-- 2. From inside the open explorer: same key, another folder, same explorer.
local lmap = vim.fn.maparg("<leader>fd", "n", false, true)
check(lmap.desc == "Find folder (scout)", "<leader>fd works in the explorer window")
lmap.callback()
s = opened("scout_dirs")
list = pick_list(s, "widget")
check(list[1] == "src/widget", "\"widget\" finds src/widget: " .. tostring(list[1]))
s:action("confirm")
e, cur = revealed(repo .. "/src/widget")
check(cur and cur.file == repo .. "/src/widget", "second pick is revealed: " .. tostring(cur and cur.file))
check(#Snacks.picker.get({ source = "explorer" }) == 1, "one explorer, not two")
check(popups <= 2, "explorer reopened at most once (popup closed on focus loss): " .. popups)
close_all()

-- 3. A sidebar explorer that stays open is reused, not reopened.
popups = 0
local side = Snacks.explorer({ cwd = repo })
vim.wait(3000, function() return #side:items() > 0 and not side:is_active() end, 20)
vim.cmd("ScoutFolders other")
s = opened("scout_dirs")
idle()
check(s and s:items()[1] and s:items()[1].file == "a/b/c/d/e/f/g/h/other", ":ScoutFolders prefills the query")
s:action("confirm")
e, cur = revealed(repo .. "/a/b/c/d/e/f/g/h/other")
check(cur ~= nil, ":ScoutFolders pick is revealed")
check(e == side and popups == 0, "the open sidebar is reused")
close_all()

-- 4. A key the user already maps is left alone.
vim.keymap.set("n", "<leader>fd", "<cmd>echo 'mine'<cr>", { desc = "Mine" })
require("scout").setup({ index_on_start = false })
check(vim.fn.maparg("<leader>fd", "n", false, true).desc == "Mine", "a user's <leader>fd is not taken over")

vim.fn.system({ util.bin(), "stop", "--root", repo })
print(failures == 0 and "ALL PASSED" or (failures .. " FAILED"))
vim.cmd((failures == 0 and "qa!" or "cq!"))
