-- Single-slot scalarization of a zero-return dynamic call. The missing first
-- value is nil; reading it must not raise an out-of-bounds host error.

local function zero()
  mark("zero")
end

local function scalar(callee)
  local value = callee()
  mark("scalar-tail")
  return value
end

local value = scalar(zero)
mark("done")
return value
