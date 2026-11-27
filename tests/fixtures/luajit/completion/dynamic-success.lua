-- Success control. The ordinary call keeps the caller's tail, pcall reports
-- true, and the original table comes back.

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

local payload = {}
local ok, result = outer(leaf, payload)
mark("done")
return ok, result
