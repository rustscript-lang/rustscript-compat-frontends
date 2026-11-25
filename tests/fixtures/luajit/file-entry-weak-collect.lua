local function sample()
  local weak = {}
  local mode = {}
  local held = {}
  mode.__mode = 'v'
  setmetatable(weak, mode)
  weak[1] = held
  weak[2] = {}
  collectgarbage('collect')
  return rawlen(weak)
end
return sample()
