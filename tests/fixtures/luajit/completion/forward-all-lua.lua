-- All-Lua multi-level forwarding control: every value that reaches a dynamic
-- callee position is a function literal declared in this chunk. The chunk
-- forwards `leaf` through `forward` and `invoke`; only `invoke` captures a
-- chunk local, and it is not used directly after `forward` is created.

local function leaf(value)
  mark("leaf")
  note(value)
  return value
end

local function invoke(callee, value)
  mark("invoke")
  return callee(value)
end

local function forward(callee, value)
  mark("forward")
  return invoke(callee, value)
end

local ok, value = pcall(forward, leaf, 7)
mark("done")
return ok, value
