-- Every UI component, for tests (not bundled). To try it, copy it to
-- plugins/ next to the config, or set [plugin.kitchen-sink] path, and
-- place it with [app.<id>] widget = "kitchen-sink". Set image to a local
-- file in the placement options to include your own picture.
local ui = dock.ui
local anim = require 'anim'
local enabled, value, clicks, image = true, 25, 0, ''
-- A made-up track: 3 minutes, playing from when the plugin loaded.
local length, position, at, playing = 180, 0, os.time(), true
-- The ring eases to each new value; `step` moves it a quarter on.
local ring = 0.25
local function set(v) value = v; dock.redraw() end
local function toggle(v) enabled = v; dock.redraw() end
local function click() clicks = clicks + 1; dock.redraw() end
local function step() ring = (ring + 0.25) % 1.25; dock.redraw() end
-- Where the track is now.
local function now() return playing and position + os.time() - at or position end
local function play()
  position, at, playing = now(), os.time(), not playing
  dock.redraw()
end
local function seek(v) position, at = v, os.time(); dock.redraw() end
-- A spinner that turns on its own: frames sampled once, played by the dock.
local function spinner(size)
  return ui.canvas{key='spin', frames=24, loop=1.2, width=size, height=size,
    draw=anim.spinner{fill=dock.theme.surface}}
end
local function popup()
  local list = { gap = 3 }
  for i = 1, 16 do list[i] = ui.text{'Scroll row ' .. i} end
  local live = playing and { rate = 1, at = at } or nil
  return ui.col{width=300, padding=12, gap=6,
    ui.row{ui.app_icon{width=24,height=24}, ui.spacer{}, ui.text{'Widget controls',style='title'}},
    ui.grid{columns={'1fr','1fr'}, ui.icon{name='go-next'}, ui.image{src=image,width=32,height=32}},
    ui.stack{height=32, ui.canvas{viewbox={32,32}, draw=function(c)
      c:circle{x=16,y=16,r=14,fill=dock.theme.accent}
    end}, ui.box{variant='badge',ui.text{clicks}}},
    ui.divider{}, ui.toggle{key='enable',value=enabled,on_change=toggle},
    ui.button{key='count',label='Count',enabled=enabled,on_click=click},
    ui.slider{key='value',value=value,max=100,enabled=enabled,on_change=set,on_commit=set},
    ui.progress{value=value,max=100},
    -- Live: these move on their own, without a redraw.
    ui.row{gap=8, align='center',
      ui.button{key='play', icon=playing and 'media-playback-pause' or 'media-playback-start',
        on_click=play},
      ui.slider{key='seek', grow=1, value=position, max=length, live=live, on_commit=seek},
    },
    ui.progress{value=position, max=length, live=live},
    -- Animated: a spinner, a pulse, a ring easing to each new value, a
    -- sweeping bar and an eased one.
    ui.row{gap=8, align='center',
      spinner(32),
      ui.canvas{key='pulse', frames=16, loop=0.8, mirror=true, width=32, height=32,
        draw=anim.pulse{}},
      ui.canvas{key='ring', frames=60, value=ring, transition=300, width=32, height=32,
        draw=anim.ring{}},
      ui.button{key='step', label='Step', on_click=step},
    },
    ui.progress{key='busy', indeterminate=true},
    ui.progress{key='eased', value=ring, transition=300},
    ui.scroll{key='list',height=70,ui.col(list)}
  }
end
-- The icon: the app's, with the spinner at its corner.
local function icon()
  return ui.stack{align='bottom-right', ui.app_icon{}, spinner('45%')}
end
-- The label counts clicks: a redraw for it keeps the spinner's phase.
local function label() return 'Clicked ' .. clicks end
return {setup=function(opts) image=opts.image or '' end, popup=popup, icon=icon, label=label}

