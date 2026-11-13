# wl-shell

Wayland shell utilities that share one runtime, each built and shipped on its
own:

- [`wl-expose`](apps/wl-expose): a window overview (exposé)
- [`wl-switcher`](apps/wl-switcher): an Alt+Tab app and window switcher

```sh
cargo build --release -p wl-expose      # just one program
cargo install --path apps/wl-switcher
```

## Layout

| Crate             | What it is                                                                                                                                                                                                                   |
| ----------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `wl-shell-core`   | The Wayland runtime: `Shell<P>` owns the connection, globals, outputs, seats, the toplevel list and its focus order (`mru-event-stream`), and implements every protocol handler. Programs implement `Program`. Also captures (dmabuf/shm). |
| `wl-shell-daemon` | The client/daemon protocol over a Unix socket and the daemon loop. Programs implement `Daemon`.                                                                                                                              |
| `wl-shell-render` | Software rendering into premultiplied ARGB8888 images: text, shadows, app icons, and a worker thread for the slow parts. No Wayland.                                                                                         |
| `wl-shell-config` | Config files: lookup, reloading, `[appearance]` with dark/light overrides, the desktop's color scheme, validators.                                                                                                           |
| `wl-shell-log`    | Diagnostics prefixed with the program's name.                                                                                                                                                                                |

A program is its surfaces and its behaviour: it reacts to the shell's events
(`Program` hooks), creates surfaces and captures through `Core`, and serves
requests (`Daemon`). It never implements Wayland handlers itself.

