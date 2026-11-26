-- A real host observer returns the boolean false. That ordinary host value is
-- not a completion status: both the observer's tail and the caller's tail run.

local function leaf(payload)
  mark("leaf")
  local observed = note(payload)
  mark("leaf-tail")
  return payload
end

local function middle(callee, payload)
  mark("middle")
  local value = callee(payload)
  mark("middle-tail")
  return value
end

local payload = {}
local value = middle(leaf, payload)
mark("done")
return value
