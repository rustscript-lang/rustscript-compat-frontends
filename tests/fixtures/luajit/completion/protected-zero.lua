-- Protected tail position: pcall adds its success value, so zero callee
-- values become exactly one value (true).

local function zero()
  mark("zero")
end

local function forward(callee)
  return pcall(callee)
end

return forward(zero)
