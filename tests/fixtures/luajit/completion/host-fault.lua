-- A real host VmError must stop the VM. pcall only catches the private Lua
-- completion, so no marker after the fault runs and no guest tag is produced.

local function leaf()
  mark("leaf")
  fault()
  mark("leaf-tail")
end

local function middle(callee)
  mark("middle")
  local value = callee()
  mark("middle-tail")
  return value
end

local function outer(callee)
  mark("outer")
  local ok, result = pcall(middle, callee)
  mark("inner-caught")
  return ok, result
end

return outer(leaf)
