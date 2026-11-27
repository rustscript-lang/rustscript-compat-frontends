-- A real published typed head whose original `__call` Arc is read from the
-- heap, not from a callable argument.
--
-- `target` and `alternate` are two real top-level zero-capture Lua closures.
-- They have different prototypes but the same concrete declared schema
-- `(Array<Int>, Resource(lua.userdata)) -> Bool`, and each has its own marker
-- so an actual child frame identifies which instance ran. `other_resource` is
-- the same arity and result with `lua.table` as its second declared resource
-- key; it is a real registered typed token, not a mock or `Unknown`.
--
-- `fault()` builds, in this same VM, two real typed userdata A and B whose
-- shared metatable stores `__gc = recv`, clears one independent weak-value
-- orphan, finishes the published Release tail, and returns A's real typed
-- token. The private plugin then inserts, at chunk level, the exact static
-- `bind(recv, ...)`: the positive arm installs `target`, the reader-replacement
-- control installs `alternate`, and the wrong-signature control installs
-- `other_resource`.
--
-- `parent` first asks the exact typed host `note` for `target`. That is the
-- old fixture selection: it is still present in the frame, but it is not the
-- value the dynamic call uses. The private plugin then inserts the exact
-- static `produce(recv, arg2)`, which has no callable argument at all: it
-- reads the real published head, the current userdata metatable's raw `__gc`,
-- and one layer of raw `__call`, and returns that stored Arc. Core validates
-- the real return against the exact callable schema. The same plugin inserts
-- the read-only `admit` seam, optionally `swap(recv, alternate)` for the
-- post-read replacement control, and the exclusive `commit` before the
-- parent's own `CallValue(2)`. The call itself still uses the same two
-- parameter slots, retargeted onto the producer's typed local.

local function target(recv, ud)
  mark("target-enter")
  return true
end

local function alternate(recv, ud)
  mark("alternate-enter")
  return true
end

local function other_resource(recv, ud)
  mark("other-enter")
  return true
end

local function first(recv, ud)
  return true
end

local function single(recv)
  return true
end

local function parent(cb, first, second, single, other, recv, arg2, wrong, mode)
  mark("parent-enter")
  local ignored = note(mode, cb, first, second, single, other)
  cb(recv, arg2)
  mark("parent-after")
  return true
end

local mode = mark("chunk-mode")
local recv = tick("recv")
local ud = fault()
local selected = parent(target, first, alternate, single, other_resource, recv, ud, ud, mode)
mark("done")
return selected, recv
