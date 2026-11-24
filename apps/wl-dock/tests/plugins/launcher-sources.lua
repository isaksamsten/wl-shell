-- The launcher sources of PLUGINS.md's examples, for tests (not
-- bundled): SSH hosts from `~/.ssh/config` behind `>`, and a calculator
-- behind `=`. To try it, set [plugin.launcher-sources] path to this file.
-- Options: `ssh_config` reads another file than ~/.ssh/config; `notes`
-- names a file offered behind `#` (a file item: Tab gives its file
-- actions); with `dry = true`, Enter copies the command it would run
-- instead of running it. A calculator query starting with `slow`
-- answers 300 ms late, like a plugin that takes its time.
local config = '~/.ssh/config'
local notes = nil
local dry = false

local function run(argv)
  if dry then
    dock.clipboard(table.concat(argv, ' '))
  else
    dock.run(argv)
  end
end

local function hosts()
  local out = {}
  for host in ('\n' .. (dock.read(config) or '')):gmatch('\nHost%s+([^%s*]+)') do
    out[#out + 1] = { id = host, title = host, icon = 'network-server' }
  end
  return out
end

local function calculate(query, emit)
  local f = load('return ' .. query, 'calc', 't', { math = math })
  local ok, v = pcall(f or error)
  if ok and v then
    emit{ { id = 'result', title = tostring(v), subtitle = query } }
  else
    emit{}
  end
end

return {
  setup = function(opts)
    config = opts.ssh_config or config
    notes = opts.notes
    dry = opts.dry == true
  end,
  sources = {
    { id = 'ssh', name = 'SSH', trigger = '>', icon = 'network-server',
      items = hosts,
      run = function(item) run{ 'foot', 'ssh', item.id } end },
    { id = 'calc', name = 'Calculator', trigger = '=', icon = 'accessories-calculator',
      search = function(query, emit)
        local slow = query:match('^slow%s*(.*)$')
        if slow then
          dock.timer(300, function() calculate(slow, emit) end)
        else
          calculate(query, emit)
        end
      end,
      run = function(item) dock.clipboard(item.title) end },
    { id = 'notes', name = 'Notes', trigger = '#',
      items = function()
        if not notes then return {} end
        return { { kind = 'file', id = 'notes', title = 'Notes', path = notes } }
      end },
  },
}

