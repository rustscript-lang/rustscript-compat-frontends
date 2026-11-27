-- Argument order and side effects at the dynamic call boundary. The two host
-- ticks must run left to right, before the callee, and the callee must run
-- exactly once. Return values are not padded.

local function leaf(first, second)
  mark("leaf")
  note(first)
  return second
end

local function middle(callee)
  mark("middle")
  local value = callee(tick("first"), tick("second"))
  mark("middle-tail")
  return value
end

local value = middle(leaf)
mark("done")
return value
