-- The same callable transport with one different actual argument.
--
-- This file is byte-for-byte the same structure as
-- `active_typed_resource_transport.lua`; the only change is the initializer of
-- `arg2`, which is the real `lua.table` token instead of the real
-- `lua.userdata` token. The compiled declarations of `target`,
-- `other_resource` and `parent` are unchanged, so the Core argument check at
-- the actual `CallValue(2)` is exercised with a correctly declared callable
-- and a different registered typed token.

local function target(recv, ud)
  mark("target-enter")
  return true
end

local function other_resource(recv, ud)
  mark("other-enter")
  return true
end

local function first(recv, ud)
  return true
end

local function second(recv, ud)
  return true
end

local function single(recv)
  return true
end

local function parent(cb, first, second, single, other, recv, arg2, mode)
  mark("parent-enter")
  local picked = note(mode, cb, first, second, single, other)
  cb(recv, arg2)
  mark("parent-after")
  return true
end

local mode = mark("chunk-mode")
local recv = tick("recv")
local ud = fault()
local table_token = mark("table-token")
local selected = parent(target, first, second, single, other_resource, recv, table_token, mode)
mark("done")
return selected, recv, table_token
