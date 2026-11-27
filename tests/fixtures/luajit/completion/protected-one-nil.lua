-- Protected tail position: a single explicit nil becomes true plus that nil.

local function one_nil()
  mark("nil")
  return nil
end

local function forward(callee)
  return pcall(callee)
end

return forward(one_nil)
