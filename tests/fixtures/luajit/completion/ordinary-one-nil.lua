-- Ordinary tail position: a single explicit nil is one value, not zero.

local function one_nil()
  mark("nil")
  return nil
end

local function passthrough(callee)
  return callee()
end

return passthrough(one_nil)
