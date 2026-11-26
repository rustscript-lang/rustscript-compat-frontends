-- Negative admission control: a host function value is passed into the
-- dynamic-call position. The lowerer must refuse instead of guessing from the
-- array's shape or reading a host value as a completion.

local function middle(callee, payload)
  return callee(payload)
end

local payload = {}
return middle(note, payload)
