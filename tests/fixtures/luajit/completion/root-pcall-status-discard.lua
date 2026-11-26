-- Roots-mode status-only pcall with distinct issued tables. Only the status
-- local is bound, so the first error object is discarded. The exact temp root
-- must be cleared before the next handoff, the discarded table must be
-- collected by the next real window, and the second payload must stay rooted
-- in the formal slot.

local function first_fail()
    mark("first")
    local discarded = newtable()
    error(discarded, 0)
end

local function second_fail()
    mark("second")
    local payload = newtable()
    error(payload, 0)
end

mark("outer")
local ok = pcall(first_fail)
mark("after-first")
local ok2, err2 = pcall(second_fail)
mark("after-second")
note(err2)
return ok, ok2
