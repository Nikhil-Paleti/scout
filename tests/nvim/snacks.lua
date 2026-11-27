-- Headless test of the snacks pickers. Needs a snacks.nvim checkout:
--   SNACKS_DIR=/path/to/snacks.nvim nvim --headless -u NONE -c "luafile tests/nvim/snacks.lua"
local here = vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":p:h:h:h")
assert(vim.env.SNACKS_DIR, "set SNACKS_DIR to a snacks.nvim checkout")
vim.opt.rtp:prepend(vim.env.SNACKS_DIR)
vim.opt.rtp:prepend(here)
vim.cmd("runtime plugin/scout.lua")

local uv = vim.uv or vim.loop
local tmp = vim.fn.tempname()
local repo = tmp .. "/repo"
vim.fn.mkdir(repo .. "/src/widget", "p")
vim.fn.writefile({ "int hello_world(void) { return 1; }", "int main() { return hello_world(); }" }, repo .. "/src/a.c")
vim.fn.writefile({ "void draw_widget(void) {}", "// hello_world lives in a.c" }, repo .. "/src/widget/draw.c")
vim.fn.writefile({ "hello_world notes" }, repo .. "/README.md")
vim.fn.system({ "git", "init", "-q", repo })
repo = uv.fs_realpath(repo)
vim.env.SCOUT_CACHE_DIR = tmp .. "/cache"
vim.cmd("cd " .. repo)

local failures = 0
local function check(cond, msg)
  if not cond then
    failures = failures + 1
  end
  io.stdout:write((cond and "ok   " or "FAIL ") .. msg .. "\n")
end

require("snacks").setup({ picker = { enabled = true } })
require("scout").setup({ route_lazyvim_keys = false, index_on_start = false })
local util = require("scout.util")
vim.fn.system({ util.bin(), "build", "--quiet" })
check(vim.v.shell_error == 0, "scout build")
local pickers = require("scout.pickers.snacks")
check(pickers.available(), "snacks picker available")

-- Waits for the picker's finder to finish; returns its (filtered) items.
local function settle(p)
  vim.wait(5000, function()
    return not p:is_active() and #p:items() > 0
  end, 20)
  vim.wait(200)
  return p:items()
end
local function files_of(items)
  local out = {}
  for _, it in ipairs(items) do
    out[#out + 1] = it.file .. (it.pos and (":" .. it.pos[1]) or " [file]")
  end
  return out
end
local function has(list, v)
  return vim.tbl_contains(list, v)
end

-- :ScoutFiles: every path, fuzzy matched by snacks.
local p = pickers.files()
local items = settle(p)
local paths = files_of(items)
check(#items == 3, "files picker lists 3 paths, got " .. #items)
check(has(paths, "src/widget/draw.c [file]"), "files picker has src/widget/draw.c")
p:close()
p = pickers.files({ search = "wdgdraw" })
items = settle(p)
check(#items == 1 and items[1].file == "src/widget/draw.c", "pattern wdgdraw keeps draw.c, got " .. table.concat(files_of(items), ", "))
p:action("confirm")
vim.wait(1000, function() return vim.api.nvim_buf_get_name(0):find("draw%.c$") ~= nil end, 20)
check(vim.api.nvim_buf_get_name(0) == repo .. "/src/widget/draw.c", "confirm opens draw.c")

-- Live search: file-name hits first, then lines with positions.
vim.cmd("edit " .. repo .. "/src/a.c")
p = pickers.live({ search = "widget" })
items = settle(p)
paths = files_of(items)
check(paths[1] == "src/widget/draw.c [file]", "live: file-name hit first, got " .. tostring(paths[1]))
check(has(paths, "src/widget/draw.c:1"), "live: line hit with a position")
p:close()

p = pickers.live({ search = "hello_world" })
items = settle(p)
paths = files_of(items)
check(has(paths, "src/a.c:1") and has(paths, "src/a.c:2") and has(paths, "README.md:1"), "live: hello_world lines, got " .. table.concat(paths, ", "))
local hit
for _, it in ipairs(items) do
  if it.file == "src/a.c" and it.pos[1] == 2 then
    hit = it
  end
end
check(hit and hit.pos[2] == 20 and hit.positions and #hit.positions == 11, "live: column and match highlight")
p:close()

-- :ScoutDir limits to the current file's directory.
vim.cmd("edit " .. repo .. "/src/widget/draw.c")
p = pickers.live({ dir = "src/widget", search = "hello_world" })
items = settle(p)
paths = files_of(items)
check(#paths == 1 and paths[1] == "src/widget/draw.c:2", "dir search: only src/widget, got " .. table.concat(paths, ", "))
p:close()

-- One fixed query (definitions), and jumping to a hit.
p = pickers.query("sym:hello_world")
items = settle(p)
paths = files_of(items)
check(#paths == 1 and paths[1] == "src/a.c:1", "sym: one definition, got " .. table.concat(paths, ", "))
p:action("confirm")
vim.wait(1000, function() return vim.api.nvim_buf_get_name(0):find("a%.c$") ~= nil end, 20)
check(vim.api.nvim_buf_get_name(0) == repo .. "/src/a.c" and vim.api.nvim_win_get_cursor(0)[1] == 1, "confirm jumps to src/a.c:1")

vim.fn.system({ util.bin(), "stop" })
print(failures == 0 and "ALL PASSED" or (failures .. " FAILED"))
vim.cmd((failures == 0 and "qa!" or "cq!"))
