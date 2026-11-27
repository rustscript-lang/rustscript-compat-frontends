local function leaf(value) return value end
local function outer(callee)
  local function middle()
    local function inner() return pcall(callee, callee) end
    return inner()
  end
  local nested_ok, nested = middle()
  return callee, nested_ok, nested
end
local original, ok, passed = outer(leaf)
return original, ok, passed, leaf
