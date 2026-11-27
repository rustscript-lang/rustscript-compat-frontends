-- Roots-mode scalar error. A non-table payload needs no heap root, but both
-- the ordinary caller frame and the protected target frame must still be
-- retired with the original completion arity.

local function table_arm()
    mark("table-arm")
    local original = newtable()
    local child = newtable()
    setchild(original, child)
    error(original, 0)
end

local function scalar_fail()
    mark("scalar-fail")
    error("boom", 0)
end

local function relay(callee)
    mark("relay")
    return callee()
end

mark("outer")
local ok, err = pcall(table_arm)
mark("after-table")
note(err)
local ok2, err2 = pcall(relay, scalar_fail)
mark("after-scalar")
return ok, ok2, err2
