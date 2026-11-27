-- Same published-head producer shape so the private plugin still inserts
-- produce, admit and commit, and retargets the real CallValue(2).
-- The fixture does not name the exit import or the guest collection import.
-- The plugin inserts the exit call after parent-after has borrowed A and
-- before the parent returns. The omission arm is this same source with that
-- insertion left out. Both arms still reach the plugin-inserted collection
-- before done. This source does not call either import, and it has no if.
--
-- The collection host creates a fresh weak orphan and does not root it.
-- It does not leave roots. done only observes.

local function target(recv, ud)
  mark("target-enter")
  return true
end

local function alternate(recv, ud)
  mark("alternate-enter")
  return true
end

local function other_resource(recv, ud)
  mark("other-enter")
  return true
end

local function first(recv, ud)
  return true
end

local function single(recv)
  return true
end

local function parent(cb, first, second, single, other, recv, arg2, wrong, mode)
  mark("parent-enter")
  local ignored = note(mode, cb, first, second, single, other)
  cb(recv, arg2)
  mark("parent-after")
  return true
end

local mode = mark("chunk-mode")
local recv = tick("recv")
local ud = fault()
local selected = parent(target, first, alternate, single, other_resource, recv, ud, ud, mode)
mark("done")
return selected, recv
