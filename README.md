# quickbar

A small shell for wlroots-family compositors (Hyprland, sway, niri, river): a
configurable border around the screen, a script-driven status bar that slides
out of the top edge, and a notification daemon that shows `notify-send`
notifications growing out of the bottom edge.

The border and the content that slides out of it are drawn into one buffer, so
they cannot drift apart or show a seam.

Not for X11, and not for compositors without `zwlr_layer_shell_v1`.

## Build

Needs `libwayland-client` and `libxkbcommon` development headers, and a Rust
toolchain.

```sh
cargo build --release
```

## Run

```sh
quickbar            # the shell
```

It needs a Wayland session. `quickbar --preview` renders the two edges to
`preview-top.png` and `preview-bottom.png` instead, which works without a
compositor.

Bind a key in your compositor to toggle the bar, for example in Hyprland:

```
bind = SUPER, B, exec, quickbar toggle-bar
```

Other commands: `show-bar`, `hide-bar`, `dnd on|off`, `reload`, `quit`.

## Configure

`$XDG_CONFIG_HOME/quickbar/config.toml` (`~/.config/quickbar/config.toml`).
Every field has a default; a missing file is fine.

```toml
[border]
width = 6
color = "#3b4252"

[bar]
height = 28
background = "#1e1e2eee"
font_size = 13
foreground = "#d8dee9"
hover_delay_ms = 150
hide_delay_ms = 400
content_max_height = 400

[[bar.module]]
name = "clock"
exec = "date '+%H:%M'"
interval = 1          # run every second, show its last line
align = "center"      # left (default) | center | right

[[bar.module]]
name = "battery"
exec = "~/bin/battery" # a long-running script
stream = true          # show every line as it arrives

[notifications]
max_visible = 3
timeout_ms = 5000
background = "#1e1e2eee"
text_color = "#d8dee9"
urgent_color = "#bf616a"
```

A module command's output is either a JSON object on each line:

```json
{"text": "78%", "color": "#a3be8c"}
```

or just plain text, which is shown in the bar's foreground colour. Blank lines
are ignored, so a script may print nothing and leave the module as it was.

## Notes

- The shell takes the `org.freedesktop.Notifications` name on the session bus.
  It exits with an error if another daemon (dunst, mako, swaync) already has it,
  rather than running alongside it silently.
- The border is drawn over whatever is underneath and reserves no space
  (`exclusive_zone` 0). Expanded status bars cover the top of windows; tiling
  layouts are not pushed down by them.

## Acceptance checklist

The rendering and all the state logic are covered by `cargo test`, but a
compositor is required for the rest. On a real session, check:

1. **Pointer passthrough.** Click and drag on a window that reaches the screen
   edges, away from the border. Transparent areas of the shell's surfaces must
   not swallow the pointer. This is the one thing no test can cover: it depends
   on the compositor honouring `wl_surface.set_input_region`.
2. **Border.** Four edges, in the configured width and colour.
3. **Slide out.** Rest the pointer on the top edge; the bar slides out of the
   border with no seam. Move away; it slides back after the delay.
4. **Quick pass.** Sweep the pointer across the top edge; the bar must not
   appear.
5. **Key binding.** `quickbar toggle-bar` shows and hides it, and the pointer
   moving away does not hide it again.
6. **Notifications.** `notify-send "hello" "body"` grows a card out of the
   bottom border. `notify-send -t 1000 ...` disappears after about a second.
   Clicking a card removes it.
7. **Modules.** Change what a module script prints; the bar updates within one
   interval.
8. **Do not disturb.** `notify-send` shows nothing after `quickbar dnd on`.
9. **Reload.** `quickbar reload` after editing the config. A broken config is
   reported and the running one is kept.
10. **Side border pointer cost.** The left and right border strips accept the
    pointer, as the top one does. Decide whether that is acceptable; if not, the
    side strips should stop accepting input.
