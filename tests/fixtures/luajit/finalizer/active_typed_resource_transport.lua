-- A top-level zero-capture Lua closure with a concrete transport prototype.
--
-- Every function here is a real Lua function of this file. `target` and
-- `other_resource` have the same arity, the same declared first parameter
-- (`Array<Int>`, the shape of the issued LuaHeap carrier) and the same
-- declared result (`Bool`). They differ only in the declared resource key of
-- their second parameter: `lua.userdata` for `target`, `lua.table` for
-- `other_resource`. The private test plugin declares those schemas on the
-- chunk-level closure bindings, so the compiled prototypes themselves carry
-- them and the Core return check compares real declarations.
--
-- `parent` receives the written candidate `cb` and the second argument
-- `arg2`, asks the exact typed host `note` for the selected callable, and
-- invokes the host-returned callable through the retargeted typed local. In
-- the positive run `arg2` is the real `lua.userdata` token issued by
-- `fault()`; a sibling fixture differs only in the initializer of that one
-- argument (`local arg2 = table_token`), so the same declared callable is
-- handed a real `lua.table` token at the actual call boundary.
--
-- `mark` is the frame marker and the real `lua.table` token issuer in the
-- second fixture. `tick` issues one real LuaHeap carrier; `fault` issues one
-- real typed `lua.userdata` token; `note` returns the original guest callable
-- through the exact two-parameter callable return schema. Nothing is collected
-- or closed, and no finalizer queue is involved.

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
local selected = parent(target, first, second, single, other_resource, recv, ud, mode)
mark("done")
return selected, recv, table_token
