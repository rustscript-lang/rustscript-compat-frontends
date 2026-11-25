local weak = {}
local mode = {}
local held = {}
mode.__mode = 'v'
setmetatable(weak, mode)
held.self = held
weak[1] = held
do
  local orphan = {}
  orphan.self = orphan
  weak[2] = orphan
end
collectgarbage('collect')
held.self = held
return rawlen(weak)
