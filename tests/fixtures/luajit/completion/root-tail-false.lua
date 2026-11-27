-- Roots-mode tail return of the data value false. The first completion slot
-- true says the call returned; the second slot false is a normal value, not an
-- error status.

local function table_arm()
    mark("table-arm")
    local original = newtable()
    local child = newtable()
    setchild(original, child)
    error(original, 0)
end

local function data_false()
    mark("data-false")
    return false
end

mark("outer")
local ok, err = pcall(table_arm)
mark("after-table")
return data_false()
