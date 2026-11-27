-- A real parent frame whose exact typed callable is also the live raw
-- `__call` of the published head's finalizer receiver.
--
-- `target` and `alternate` are top-level zero-capture Lua closures with the
-- concrete `(Array<Int>, Resource(lua.userdata)) -> Bool` prototype. `recv` is
-- one real issued LuaHeap table carrier. `fault()` builds, in this same VM,
-- two real typed userdata A and B whose shared metatable stores `__gc = recv`,
-- clears one independent weak-value orphan, finishes the published Release
-- tail, and returns A's real typed token. The private plugin then inserts, at
-- chunk level, the exact static `bind(recv, target)` which installs `target`
-- as `recv`'s own metatable's raw `__call`, and `wrong_token()` which returns
-- B's real typed token: a legal `lua.userdata` handle naming another object.
--
-- `parent` asks the exact typed host `note` for `target`. The private plugin
-- inserts the exact static `admit` (the `Borrow/Value/Borrow` typed seam)
-- after that local, optionally `swap(recv, alternate)` for the raw-edge-change
-- control, then the exact static `commit` immediately before the parent's own
-- `CallValue(2)`. The call itself still uses the same local and the same two
-- parameter slots.

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
  local picked = note(mode, cb, first, second, single, other)
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
