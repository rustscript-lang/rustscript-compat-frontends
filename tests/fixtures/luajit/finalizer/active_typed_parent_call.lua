-- An active Lua parent frame takes back the callable that an exact typed host
-- returns and invokes it in the same frame and the same VM.
--
-- Every function here is a real Lua function of this file. The parent's own
-- dynamic call site is written against `cb`, the parameter the completion
-- lowerer statically proves; that same call node is the one the private test
-- plugin retargets to the typed local that holds the host-returned callable.
-- No RSS, bytecode or hand-written callable stands in for the Lua function.
--
-- This pin's private completion lowerer admits only four guest host-call names
-- (`mark`, `note`, `fault`, `tick`) and one arity per name, so the private test
-- catalog binds those four names to the four adapters this witness needs:
--
--   mark(label)   records one active-frame marker and returns the integer the
--                 test staged for this arm. In a two-parameter callee frame it
--                 also records the declared formal slots, the receiver class
--                 and revalidates the `lua.userdata` token it finds there.
--   tick(label)   returns one freshly issued LuaHeap table carrier.
--   fault()       returns one freshly issued `resource<lua.userdata>` token.
--   note(mode, target, first, second, single, other)
--                 returns the selected original guest callable through a
--                 two-parameter `Callable` return schema.
--
-- `decoy` and `other` are two chunk-level Lua functions of the same declared
-- shape whose bodies report different captured values. `make` builds two real
-- instances of one nested `callback` prototype with different captures, so the
-- observation separates "one prototype" from "one instance" even though a
-- nested closure carries no compiled callable schema in this pin. `single` has
-- one parameter, so returning it through the `note` return schema exposes the
-- runtime signature check.
--
-- The five candidates are written at the call site; `mode` only selects which
-- of them the host hands back, and the host always returns the value the VM
-- itself produced.

local tag = "parent-tag"
local other_tag = "other-tag"

local function decoy(recv, ud)
  mark(tag)
  return 0
end

local function other(recv, ud)
  mark(other_tag)
  return 0
end

local function single(recv)
  return 0
end

local function make(name)
  local function callback(recv, ud)
    mark(name)
    return 0
  end
  return callback
end

local function parent(cb, first, second, single, other, recv, ud)
  mark("parent-enter")
  local mode = mark("mode")
  local picked = note(mode, cb, first, second, single, other)
  cb(recv, ud)
  mark("parent-after")
  return picked
end

local recv = tick("recv")
local ud = fault()
local first = make("first")
local second = make("second")
local selected = parent(decoy, first, second, single, other, recv, ud)
mark("done")
return selected, decoy, other, first, second, recv, ud
