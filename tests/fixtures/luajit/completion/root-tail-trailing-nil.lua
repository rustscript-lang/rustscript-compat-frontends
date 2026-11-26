-- Roots-mode tail return with a trailing nil. The completion arity is two;
-- the second value is nil and must not be dropped while the call's root frame
-- is retired.

local function table_arm()
    mark("table-arm")
    local original = newtable()
    local child = newtable()
    setchild(original, child)
    error(original, 0)
end

local function pair()
    mark("pair")
    return 1, nil
end

mark("outer")
local ok, err = pcall(table_arm)
mark("after-table")
return pair()
