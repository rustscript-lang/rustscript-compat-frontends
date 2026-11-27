local function f(x) return x end
local function other(x) return x end
local function outer()
  local function inner() return f(f) end
  return inner()
end
local direct = f(f)
local nested = outer()
return direct, nested, f, other
