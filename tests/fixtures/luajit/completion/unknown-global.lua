-- Negative control: an unknown global call is an unsupported input and is
-- rejected at lowering, never lowered as a silent nil call.

local payload = {}
return missing(payload)
