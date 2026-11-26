-- Roots-mode normal completion carrying an issued table value. No root slot
-- is reserved for a table in a successful completion in this slice, so the
-- handoff must refuse it explicitly instead of returning a carrier that a
-- later collection can reclaim.

local function make()
    mark("make")
    local table_value = newtable()
    local child = newtable()
    setchild(table_value, child)
    return table_value
end

mark("outer")
return make()
