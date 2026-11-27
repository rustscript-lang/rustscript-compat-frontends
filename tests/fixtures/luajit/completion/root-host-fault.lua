-- Roots-mode real host fault after a successful protected table receive. The
-- fault must abort through the Core error path, not become a guest completion
-- tag, and no marker after the fault may run.

local function leaf()
    mark("leaf")
    local original = newtable()
    local child = newtable()
    setchild(original, child)
    error(original, 0)
end

mark("outer")
local ok, err = pcall(leaf)
mark("after-catch")
fault()
return ok, err
