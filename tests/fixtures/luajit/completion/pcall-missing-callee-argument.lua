-- Admission counterexample through pcall forwarding: `pcall(invoke)` has no
-- forwarded argument, so `invoke`'s callable parameter is padded with nil.
-- The earlier `pcall(invoke, leaf)` must not make the parameter look pure.

local function leaf()
  mark("leaf")
  return "leaf"
end

local function invoke(f)
  mark("invoke")
  return f()
end

local ok1, value1 = pcall(invoke, leaf)
local ok2, value2 = pcall(invoke)
mark("done")
return ok1, value1, ok2, value2
