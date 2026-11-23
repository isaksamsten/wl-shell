-- The clock: shows the time as its item's label.
--
--   [app.<id>]
--   widget = { plugin = "clock", format = "%H:%M:%S" }
--
-- `format` is an `os.date` format (default "%H:%M").

local C = {}
local format = '%H:%M'

function C.setup(opts)
  if type(opts.format) == 'string' then format = opts.format end
  -- The dock only redraws the label when its text changes.
  dock.every(1000, dock.redraw)
end

function C.label()
  return os.date(format)
end

return C

