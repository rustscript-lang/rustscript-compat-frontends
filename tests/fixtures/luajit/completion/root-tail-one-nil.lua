-- Roots-mode tail return of exactly one nil. The protected table arm keeps the
-- completion transport and the formal root exercised in the same real file;
-- the tail call must retire its frame and keep arity one.

local function table_arm()
    mark("table-arm")
    local original = newtable()
    local child = newtable()
    setchild(original, child)
    error(original, 0)
end

local function one_nil()
    mark("one-nil")
    return nil
end

mark("outer")
local ok, err = pcall(table_arm)
mark("after-table")
return one_nil()
