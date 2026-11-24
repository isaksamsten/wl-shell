-- The launcher actions of PLUGINS.md's examples, for tests (not
-- bundled): Shrink image on images, with a choice of sizes, and Follow
-- logs on running apps. To try it, set [plugin.launcher-actions] path to
-- this file. With the option `dry = true` it copies the command it would
-- run instead of running it. A file called `stuck` gets an action whose
-- choices take two seconds, longer than the launcher waits.
local dry = false

local function run(argv)
  if dry then
    dock.clipboard(table.concat(argv, ' '))
  else
    dock.run(argv)
  end
end

return {
  setup = function(opts) dry = opts.dry == true end,
  actions = {
    { id = 'shrink', name = 'Shrink image', icon = 'image-x-generic',
      when = { kind = 'file', mime = 'image/*' },
      choices = function(s)
        return { { id = '50', name = '50%' }, { id = '25', name = '25%' } }
      end,
      run = function(s, choice)
        run{ 'magick', s.path, '-resize', choice.id .. '%', s.path .. '.small.png' }
      end },
    { id = 'logs', name = 'Follow logs', icon = 'utilities-terminal',
      when = { kind = 'app', running = true },
      run = function(s) run{ 'foot', 'journalctl', '--user', '-f', '-t', s.id } end },
    { id = 'stuck', name = 'Never answers', when = { kind = 'file', path = '*/stuck' },
      choices = function()
        local t = os.clock() + 2
        while os.clock() < t do end
        return { { id = 'late' } }
      end,
      run = function() end },
  },
}

