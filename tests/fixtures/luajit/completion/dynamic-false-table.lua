-- The payload table's first item is false. That is data, not an error flag:
-- the success path still keeps `middle-tail` and pcall still reports true.

local function leaf(payload)
  mark("leaf")
  note(payload)
  return payload
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

local payload = { false }
local ok, result = outer(leaf, payload)
mark("done")
return ok, result
