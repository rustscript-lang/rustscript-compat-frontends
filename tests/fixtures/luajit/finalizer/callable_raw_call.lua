-- One issued owner metatable is reused for two real zero-capture guest
-- closures. `tick("owner")` issues a live LuaHeap table and attaches a
-- metatable whose raw `__call` record starts as the false sentinel. `note`
-- is the heap-owned fixed two-argument import; it receives the exact issued
-- owner array and one callable this VM produced.
--
-- First `note(owner, first_target)` must replace the false sentinel; the
-- second `note(owner, second_target)` must replace that record's existing
-- closure generation. All later calls are refusals: a captured closure, the
-- same-shaped but unissued lookalike array, a metatable with no `__call`
-- record, a true scalar, and a stored nil gap. The fixture returns every
-- value so the same VM still owns live Arcs after the run and the test can
-- read the actual raw edge and release it independently.
--
-- No host import here returns a callable. `tick` returns only issued tables
-- or scalar slot/generation integers; the unissued lookalike array is built
-- by this guest as a table literal with those scalar fields.

local function first_target()
  return 1
end

local function second_target()
  return 2
end

local function make(name)
  local function captured()
    return name
  end
  return captured
end

local captured_target = make("capture")

local owner = tick("owner")
local owner_slot = tick("owner-slot")
local owner_generation = tick("owner-generation")
local lookalike = { owner_slot, owner_generation }
local missing_owner = tick("missing")
local true_owner = tick("true")
local gap_owner = tick("gap")

mark("first-before")
local first_ok = note(owner, first_target)
mark("first-after")

local second_ok = note(owner, second_target)
mark("second-after")

mark("capture-before")
local captured_ok = note(owner, captured_target)
mark("capture-after")

mark("unissued-before")
local unissued_ok = note(lookalike, first_target)
mark("unissued-after")

mark("missing-before")
local missing_ok = note(missing_owner, first_target)
mark("missing-after")

mark("true-before")
local true_ok = note(true_owner, first_target)
mark("true-after")

mark("gap-before")
local gap_ok = note(gap_owner, first_target)
mark("gap-after")

mark("done")

return owner, first_target, second_target, captured_target, lookalike, missing_owner,
       true_owner, gap_owner, first_ok, second_ok, captured_ok, unissued_ok,
       missing_ok, true_ok, gap_ok
