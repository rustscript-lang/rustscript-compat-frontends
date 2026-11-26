-- Missing call arguments are padded with nil at the Lua call boundary. nil in
-- a data position is not a callable admission failure, including through
-- pcall and an ordinary forwarding call. `take` is not called directly after
-- `forward` captures it.

local function take(value)
  mark("take")
  return value
end

local missing = take()
local ok, protected = pcall(take)

local function forward(value)
  mark("forward")
  return take(value)
end

local forwarded = forward(42)
mark("done")
return missing, ok, protected, forwarded
