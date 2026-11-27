-- Roots-mode table literal. A Core array literal is not an issued heap table,
-- so it has no place in the single-issued-table root slice and must be refused
-- at lowering rather than carried as a fake table.

local payload = { 1, 2 }
return payload
