-- Admission control: the production completion lowerer refuses to emit a
-- dynamic call whose callee is a local bound from a host return.
--
-- This file is never executed. It records the real first hard block that makes
-- the witness in `active_typed_parent_call.lua` generate its typed-local call
-- node instead of loosening the general admission rule: `picked` is a local
-- bound from a host call, so it has no proven Lua provenance and no unique
-- arity, and the completion slice refuses the call at lowering time.

local tag = "unadmitted"

local function decoy(recv, ud)
  mark(tag)
  return 0
end

local function parent(cb, recv, ud)
  local mode = mark("mode")
  local picked = note(mode, cb, cb, cb, cb, cb)
  picked(recv, ud)
  return picked
end

local recv = tick("recv")
local ud = fault()
local selected = parent(decoy, recv, ud)
return selected
