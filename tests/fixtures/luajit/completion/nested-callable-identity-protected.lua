local function f(x) return x end
local function other(x) return x end
local function outer()
  local function inner() return pcall(f, f) end
  return inner()
end
local direct_ok, direct = pcall(f, f)
local nested_ok, nested = outer()
return direct_ok, direct, nested_ok, nested, f, other
