-- Dynamic explicit error across one ordinary Lua call, then caught by pcall.
-- The payload table itself is the error object (level 0). `middle-tail` must
-- never run; `inner-caught`, `outer-tail`, and `done` must run.

local function leaf(payload)
  mark("leaf")
  note(payload)
  error(payload, 0)
end

local function middle(callee, payload)
  mark("middle")
  local value = callee(payload)
  mark("middle-tail")
  return value
end

local function outer(callee, payload)
  mark("outer")
  local ok, result = pcall(middle, callee, payload)
  mark("inner-caught")
  mark("outer-tail")
  return ok, result
end

local payload = {}
local ok, result = outer(leaf, payload)
mark("done")
return ok, result
