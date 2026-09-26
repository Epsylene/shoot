# shoot

A fast screenshot tool for X11 with no popup and no window. Each shot is saved to `~/Pictures` and copied to the clipboard in the same step.

## Usage

```
shoot [screen|window|area]
```

| Mode     | Captures                                   | Default key                  |
| -------- | ------------------------------------------ | ---------------------------- |
| `screen` | The whole screen (the default)             | `Print`, `Ctrl+Shift+Super+S` |
| `window` | The active window, with its title bar      | `Alt+Print`                  |
| `area`   | A rectangle that you drag on a frozen screen | `Shift+Print`, `Shift+Super+S` |

In `area` mode, the screen freezes and becomes darker. Drag with the left button to select a rectangle. Release the button to take the shot. A click with no drag clears the selection. `Escape` or a right-click cancels.

After each shot, the captured region becomes a little darker for approximately 200 ms. This effect needs a compositor. Without one, shoot does not show it.

Files get the same names as the files from gnome-screenshot: `Screenshot from YYYY-MM-DD HH-MM-SS.png`. If the name is already in use, shoot adds ` - 2`, ` - 3` and so on.

## Clipboard

shoot puts the PNG on the `CLIPBOARD` selection as `image/png`. If a clipboard manager is running (for example `csd-clipboard` on Cinnamon), shoot gives the image to it and then exits. The image stays available after shoot stops. If no clipboard manager is running, shoot stays in the background until a different program takes the clipboard.

## Build and install

```
cargo build --release
install -Dm755 target/release/shoot ~/.local/bin/shoot
```

shoot connects to the X server directly through `x11rb`. It does not need Xlib, libxcb or other system libraries. It does not work in a Wayland session.

## Key bindings (Cinnamon)

Remove the built-in screenshot keys, then bind the shoot commands as custom shortcuts:

```
S=org.cinnamon.desktop.keybindings.media-keys
gsettings set $S screenshot "[]"
gsettings set $S window-screenshot "[]"
gsettings set $S area-screenshot "[]"

P=/org/cinnamon/desktop/keybindings/custom-keybindings
dconf write $P/custom0/name "'Screenshot (shoot)'"
dconf write $P/custom0/command "'$HOME/.local/bin/shoot'"
dconf write $P/custom0/binding "['Print', '<Primary><Shift><Super>s']"
dconf write $P/custom1/name "'Window screenshot (shoot)'"
dconf write $P/custom1/command "'$HOME/.local/bin/shoot window'"
dconf write $P/custom1/binding "['<Alt>Print']"
dconf write $P/custom2/name "'Area screenshot (shoot)'"
dconf write $P/custom2/command "'$HOME/.local/bin/shoot area'"
dconf write $P/custom2/binding "['<Shift>Print', '<Shift><Super>s']"
gsettings set org.cinnamon.desktop.keybindings custom-list "['__dummy__', 'custom0', 'custom1', 'custom2']"
```

These commands replace any custom shortcuts that already use `custom0` to `custom2`.

## Configuration

There is no configuration file. To change the darkening effect, edit the constants at the top of `src/overlay.rs` and build again:

| Constant      | Controls                                                 |
| ------------- | -------------------------------------------------------- |
| `FLASH_ALPHA` | How dark the effect after a shot starts (0 to 255)       |
| `FLASH_HOLD`  | How long the effect stays at full darkness                 |
| `FLASH_STEPS`, `FLASH_STEP` | How many steps the fade has, and how long each step is |
| `DIM`         | How bright the frozen screen is in `area` mode, out of 256 |

## Errors

If a shot fails, shoot sends a desktop notification with `notify-send` and writes the error to stderr.
