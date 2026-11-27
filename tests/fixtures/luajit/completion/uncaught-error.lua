-- An uncaught explicit error reaches the chunk boundary as the private error
-- completion: it must not be silently presented as a normal chunk result.

local function leaf(payload)
  mark("leaf")
  note(payload)
  error(payload, 0)
end

local payload = {}
return leaf(payload)
