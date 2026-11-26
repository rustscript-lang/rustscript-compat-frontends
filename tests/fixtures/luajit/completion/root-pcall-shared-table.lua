-- Roots-mode status-only pcall that reuses one issued error table. Both
-- protected calls error with the same table created at chunk level. The
-- status-only call discards the object, and the second handoff must still run
-- instead of finding the first call's temp root still occupied.

local original = newtable()
local child = newtable()
setchild(original, child)

local function first_fail()
    mark("first")
    error(original, 0)
end

local function second_fail()
    mark("second")
    error(original, 0)
end

mark("outer")
local ok = pcall(first_fail)
mark("after-first")
local ok2, err2 = pcall(second_fail)
mark("after-second")
note(err2)
return ok, ok2, err2
