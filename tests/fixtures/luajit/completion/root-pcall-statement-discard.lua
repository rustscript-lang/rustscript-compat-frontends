-- Roots-mode pcall in statement position with distinct issued tables. No
-- result is bound; the whole protected sequence is discarded. The exact temp
-- root must still be cleared before the next handoff, and the next protected
-- receive must succeed.

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
pcall(first_fail)
mark("after-first")
local ok2, err2 = pcall(second_fail)
mark("after-second")
note(err2)
return ok2
