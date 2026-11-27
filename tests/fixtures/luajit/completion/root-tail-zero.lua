-- Roots-mode tail return of a zero-value completion. The protected table arm
-- establishes two issued witness tables and a formal root; the tail call must
-- still retire its own frame with arity zero and preserve the exact chunk
-- result arity.

local function table_arm()
    mark("table-arm")
    local original = newtable()
    local child = newtable()
    setchild(original, child)
    error(original, 0)
end

local function zero()
    mark("zero")
end

mark("outer")
local ok, err = pcall(table_arm)
mark("after-table")
return zero()
