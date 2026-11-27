-- Ordinary tail position: a dynamic call returning zero values stays zero.

local function zero()
  mark("zero")
end

local function passthrough(callee)
  return callee()
end

return passthrough(zero)
