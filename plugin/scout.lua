if vim.g.loaded_scout then
  return
end
vim.g.loaded_scout = true

local function scout()
  return require("scout")
end

local cmd = vim.api.nvim_create_user_command

cmd("ScoutLive", function(a)
  scout().live({ search = a.args ~= "" and a.args or nil })
end, { nargs = "*", desc = "Scout: live search" })

cmd("Scout", function(a)
  require("scout.search").quickfix(a.args)
end, { nargs = "+", desc = "Scout: search into the quickfix list" })

cmd("ScoutWord", function()
  scout().word()
end, { range = true, desc = "Scout: word under the cursor" })

cmd("ScoutSym", function(a)
  scout().sym(a.args)
end, { nargs = "?", desc = "Scout: definitions of a name" })

cmd("ScoutCallers", function(a)
  scout().callers(a.args)
end, { nargs = "?", desc = "Scout: call sites of a name" })

cmd("ScoutIndex", function(a)
  require("scout.daemon").index(require("scout.util").root(), a.args == "rebuild")
end, {
  nargs = "?",
  complete = function()
    return { "rebuild" }
  end,
  desc = "Scout: index this project (or rebuild)",
})

cmd("ScoutStatus", function()
  scout().status()
end, { desc = "Scout: index status" })
