-- A normal completion keeps a trailing nil as one value after the first.
-- The private transport must not truncate the ordered value list.

local function pair()
  mark("pair")
  return 1, nil
end

local function passthrough(callee)
  return callee()
end

return passthrough(pair)
