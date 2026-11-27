-- Two real chunk-level Lua functions and one captured nested function are
-- handed to the heap-owned `note` import. The import derives the capture
-- count from this VM's compiled prototype for each value; the guest cannot
-- choose the count. `note` is the completion slice's fixed host name; the
-- heap-owned dispatch is what admits or refuses the callable.
--
-- The two chunk-level functions have no captures. Core's Lua closure
-- representation still attaches an empty environment container to every
-- function; the private admission checks the compiled capture count instead
-- of the container bit. `make` returns a nested function whose environment
-- carries the captured `name`; that value is the narrow capture-refusal
-- control. The import returns false for it and does not allocate a closure
-- generation.
--
-- Nothing here fabricates a callable: every value the host sees was produced
-- by this file in this VM. The chunk returns the three callables and the three
-- admission results so the same VM still owns live Arcs after the run; the
-- heap slot's own ownership can then be counted and released independently.

local function rooted_target()
  return 1
end

local function unrooted_target()
  return 2
end

local function make(name)
  local function captured()
    return name
  end
  return captured
end

mark("register-rooted")
local rooted_ok = note(rooted_target)

mark("register-unrooted")
local unrooted_ok = note(unrooted_target)

local captured = make("environment")
mark("register-environment")
local captured_ok = note(captured)

mark("done")
return rooted_target, unrooted_target, captured, rooted_ok, unrooted_ok, captured_ok
