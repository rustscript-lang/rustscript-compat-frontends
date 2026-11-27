local function f(x) return 1 end
local function outer()
  local function inner() return f(f) end
  return inner()
end
local direct = f(f)
local nested = outer()
return direct, nested
