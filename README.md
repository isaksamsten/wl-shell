# wl-shell

Wayland shell utilities that share one runtime, each built and shipped on its
own:

- [`wl-expose`](apps/wl-expose): a window overview (exposé)
- [`wl-switcher`](apps/wl-switcher): an Alt+Tab app and window switcher

```sh
cargo build --release -p wl-expose      # just one program
cargo install --path apps/wl-switcher
```
