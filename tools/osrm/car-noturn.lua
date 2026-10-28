-- OSRM's stock car.lua with its turn and U-turn penalties set to 0 (phase 3 step 3.5):
-- routing the same pairs with this and with the stock profile shows what turn costs alone
-- change. Signal, stop and give-way delays (obstacles) are kept.
package.path = '/opt/?.lua;' .. package.path
local car = dofile('/opt/car.lua')
api_version = 4

function setup()
  local p = car.setup()
  p.turn_penalty = 0
  p.properties.u_turn_penalty = 0
  return p
end

return { setup = setup, process_way = car.process_way, process_node = car.process_node,
  process_turn = car.process_turn }
