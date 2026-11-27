local function leaf(value)
  mark("leaf")
  note(value)
  return value
end

local function invoke(callee, value)
  mark("invoke")
  return callee(value)
end

local direct = invoke(leaf, 5)

local function forward(callee, value)
  mark("forward")
  return invoke(callee, value)
end

local ok, value = pcall(forward, leaf, 7)
mark("done")
return ok, value, direct
