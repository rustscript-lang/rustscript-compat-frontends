local function leaf()
    mark("leaf")
    local original = newtable()
    local child = newtable()
    setchild(original, child)
    error(original, 0)
end

local function middle(callee)
    mark("middle")
    callee()
    mark("middle-tail")
    return true
end

local function relay(mid, callee)
    mark("relay")
    return mid(callee)
end

mark("outer")
local ok, err = pcall(relay, middle, leaf)
note(err)
mark("outer-tail")
return ok
