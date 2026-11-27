-- Two real chunk-level Lua functions and one captured nested function are
-- handed to the private host import. The host registers the exact Core
-- callable Arc each guest binding produced.
--
-- The two chunk-level functions have no captures. Core's Lua closure
-- representation still attaches an empty environment container to every
-- function; the private admission checks the compiled capture count instead
-- of the container bit. `make` returns a nested function whose environment
-- carries the captured `name`; that value is the narrow capture-refusal
-- control. Nothing here fabricates a callable: every value the host sees was
-- produced by this file in this VM.
--
-- The chunk returns the three callables so the same VM still owns live Arcs
-- after the run; the heap slot's own ownership can then be counted and
-- released independently.

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
note(rooted_target)

mark("register-unrooted")
note(unrooted_target)

local captured = make("environment")
mark("register-environment")
note(captured)

mark("done")
return rooted_target, unrooted_target, captured
