-- Roots-mode uncaught issued table. The chunk completion is the explicit
-- error [false, payload]. The finish host keeps the chunk root frame open so
-- the caller receives the table together with an explicit live root, and the
-- caller is responsible for the final release and collection.

local function leaf()
    mark("leaf")
    local original = newtable()
    local child = newtable()
    setchild(original, child)
    note(original)
    error(original, 0)
end

mark("outer")
return leaf()
