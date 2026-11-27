local function f(x) return 1 end
local direct = f(f)
local function outer()
  local function inner() return f(f) end
  return inner()
end
local nested = outer()
return direct, nested
