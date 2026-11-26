-- Admission counterexample through two levels of pcall forwarding:
-- `forward(leaf)` contributes a Lua function, but `forward(false)` contributes
-- a non-Lua value to the same parameter. The non-Lua possibility must reach
-- `invoke.f`, where `f()` is the dynamic callable position.

local function leaf()
  mark("leaf")
  return true
end

local function invoke(f)
  mark("invoke")
  return f()
end

local function forward(x)
  mark("forward")
  return pcall(invoke, x)
end

local ok1, value1 = forward(leaf)
local ok2, value2 = forward(false)
mark("done")
return ok1, value1, ok2, value2
