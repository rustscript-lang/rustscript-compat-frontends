-- Admission counterexample at an ordinary dynamic call: `invoke()` omits the
-- callable argument, so the Lua call boundary pads the parameter with nil.
-- The earlier `invoke(leaf)` call must not hide that nil from the analysis:
-- `f()` inside `invoke` would become a real InvalidCallable at the VM.

local function leaf()
  mark("leaf")
  return 1
end

local function invoke(f)
  mark("invoke")
  return f()
end

local first = invoke(leaf)
local second = invoke()
mark("done")
return first, second
