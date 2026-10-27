-- OSRM's stock car.lua, unchanged, plus one line per way with a highway or route tag:
-- "W <id> <1 if routed> <tags>", to compare coverage with omp_core::profile.
package.path = '/opt/?.lua;' .. package.path
local car = dofile('/opt/car.lua')
api_version = 4
setup = car.setup
process_node = car.process_node
process_turn = car.process_turn

local keys = {'highway', 'route', 'access', 'vehicle', 'motor_vehicle', 'motorcar', 'service',
  'oneway', 'area', 'construction', 'proposed', 'maxheight', 'maxwidth', 'width', 'maxweight',
  'maxlength', 'narrow', 'smoothness', 'impassable', 'status', 'hov', 'duration', 'bridge'}

function process_way(profile, way, result, relations)
  car.process_way(profile, way, result, relations)
  if not (way:get_value_by_key('highway') or way:get_value_by_key('route')) then
    return
  end
  local function usable(m, speed)
    return m ~= mode.inaccessible and (speed > 0 or result.duration > 0)
  end
  local ok = usable(result.forward_mode, result.forward_speed)
    or usable(result.backward_mode, result.backward_speed)
  local t = {}
  for _, k in ipairs(keys) do
    local v = way:get_value_by_key(k)
    if v then t[#t + 1] = k .. '=' .. v:gsub('%s', '_') end
  end
  io.write('W ' .. way:id() .. ' ' .. (ok and 1 or 0) .. ' ' .. table.concat(t, ' ') .. '\n')
end

return { setup = setup, process_way = process_way, process_node = process_node,
  process_turn = process_turn }
