-- One issued heap table whose raw `__call` record receives two real original
-- zero-capture guest closures through the heap-owned fixed two-argument
-- `note(owner, target)` import. The same VM also keeps a real guest helper
-- (`replace_call`) so the second write can happen after the first published
-- pending-finalizer head has been dequeued, against the same metatable edge.
--
-- `tick("owner")` issues a live LuaHeap table and attaches a metatable whose
-- raw `__call` starts as the false sentinel. The first `note` replaces that
-- sentinel with `first_target`. The helper is returned to the test and is
-- invoked in this same halted VM later; it calls `note(receiver, second_target)`
-- again, so the existing import allocates the next closure generation on the
-- same raw record. Both targets are chunk-level functions with no captures.
--
-- No userdata is created here. The test creates two real typed userdata in the
-- same VM after the run, points their raw `__gc` at this receiver, and uses
-- the real private collector; this file only produces the receiver, the two
-- original closures, and the guest-side replacement helper.

local function first_target()
  return 1
end

local function second_target()
  return 2
end

local receiver = tick("owner")
local first_ok = note(receiver, first_target)
mark("ready")

local function replace_call()
  return note(receiver, second_target)
end

return receiver, first_target, second_target, replace_call, first_ok
