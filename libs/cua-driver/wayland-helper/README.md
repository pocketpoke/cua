# cua WinRects — GNOME Shell helper extension (Wayland)

A small GNOME Shell extension that lets cua-driver get **pixel coordinates**,
activate an exact target window, capture the compositor stage, and draw the
**agent cursor** on GNOME Mutter Wayland. A normal Wayland client cannot do
these things globally.

It exposes `org.cua.WinRects` on the session bus:

- `GetVersion() -> uint` — a browser-sensitive API version. cua-driver only
  accepts it after resolving the immutable D-Bus owner and proving the owner is
  the current user's system-installed `gnome-shell` process.
- `GetRects() -> json` — every window's frame geometry and surface-buffer
  origin. cua-driver combines the buffer origin with AT-SPI
  `CoordType::Window` per-widget coords: `screen = origin + window_xy`. This is
  the GNOME analogue of the X11 `_GTK_FRAME_EXTENTS` reconstruction (AT-SPI's
  `CoordType::Screen` is `(0,0)` for every widget on Mutter). Keeping the frame
  and buffer origins separate accounts for GTK client-side shadows.
- `Activate(id) -> bool` — activate one Shell stable-sequence window and report
  whether the request was accepted. cua-driver verifies focus through a second
  `GetRects` snapshot before sending focus-bound portal/libei input, preventing
  input from leaking into whichever application happened to be focused.
- `Capture() -> png_base64` — capture the compositor stage through Shell's
  screenshot API. cua-driver crops it with the same authoritative geometry.
- `MoveCursor(x,y)` / `ClickPulse(x,y)` / `HideCursor()` — position and hide
  the agent cursor as a Clutter actor on the compositor stage.
- `SetCursorState(action,delivery,target,active)` — render the same 12 semantic
  action states as the cross-platform `cua.default` cursor theme. Delivery and
  target context appears as host-owned chips in the session badge rather than
  pointer-relative theme artwork. This contract requires helper v8.
- `SetCursorColor(fill_color)` — apply the stable per-session fill selected by
  cua-driver. The helper validates the `#RRGGBB` value, updates the matching
  glow, and keeps a white pointer outline.

It runs in the shell's privileged context, so **no xdg-desktop-portal grant** is
needed (unlike libei/RemoteDesktop).

## Install

```
~/.cua-driver/packages/current/wayland-helper/install.sh
# From a source checkout, use ./install.sh in this directory.
# then log out/in once (GNOME loads extensions only at session startup)
gnome-extensions info winrects@cua   # -> State: ACTIVE
```

cua-driver auto-detects it at runtime (`wayland::shell_helper`). AX operations
still work when it is absent, but pixel geometry, the Shell cursor, and safe
foreground portal input are unavailable. cua-driver refuses focus-bound input
instead of injecting into an unverified target.

The semantic cursor requires helper v8. When an older helper is still loaded,
cua-driver does not draw its legacy cursor. Re-run the helper installer, then
reload the GNOME session so the new compositor-owned artwork becomes active.

Browser setup and consent are held to a stricter boundary: helper API v4 or
newer must be served by the verified GNOME Shell owner. The driver addresses
that owner's unique D-Bus name, so another same-session process cannot replace
the public name between verification and an activation request. One exact
target is activated only for the bounded operation, then the previously
focused Shell window is restored and verified.

wlroots compositors such as Sway and labwc do not need it: cua-driver uses
foreign-toplevel activation, virtual-pointer input, and layer-shell there.

## KDE Plasma 6 / KWin status

KDE Plasma Wayland has an explicitly opt-in experimental focus-bound input
route. The production adapter consults the KWin helper only when the bridge
owns its D-Bus name, KWin reports the helper script as loaded, and the
versioned capability/snapshot records parse successfully. It then requires an
exact `(pid, window_id)` ownership match, target-focus confirmation, and
previous-focus restoration confirmation before sending bounded input.
Without that complete handshake, cua-driver refuses instead of relying on
stale `kwinrc` records or whichever window happens to be focused.

The installed KWin 6.7.1 API was re-checked locally. Declarative scripts expose
`Workspace.windowList`, `Workspace.activeWindow`, `readConfig`, and outbound
`callDBus`. They do not expose `writeConfig`, a custom D-Bus service/object
registration API, or an inbound callback endpoint. KWin's own D-Bus API exposes
fixed scripting/window-manager methods; it does not route arbitrary requests to
a script. Therefore a declarative script alone cannot make the required
bidirectional capability/snapshot/request/response channel.

The checkout keeps the explicit bridge path as an opt-in experimental route:

- KWin package: `kwin-cua-helper/`
- Rust KWin client: `platform-linux/src/wayland/kwin_helper.rs`
- Guarded installer: `./install-kwin.sh`
- Read-only/live proof probe: `./probe-kwin.sh`
- Bridge target: `platform-linux/src/bin/kwin-helper-bridge.rs`
- Nix outputs: `kwin-helper` and `kwin-helper-bridge`

The prototype's `callDBus` publication lane requires a running
`org.cua.KWinHelper` process. `kwriteconfig6` can carry requests into the
script, but the script cannot write the response back to config by itself. No
KDE foreground input is allowed until a real capability handshake, exact
`(pid, window_id)` ownership match, target-focus confirmation, and previous
focus restoration confirmation all succeed. The activation probe is
intentionally not run by the driver or its tests.

Minimum bridge prerequisite for a future supported implementation:

1. Enter a working Rust/Nix environment with all locked sources available.
2. Build the opt-in target without relying on an automatic download:

   ```text
   nix develop --command cargo build --release -p platform-linux \
     --features kwin-helper-bridge --bin kwin-helper-bridge
   ```

`probe-kwin.sh` verifies the live capability publication and exact
activation/restore transaction before a host relies on focus-bound input. The
route remains disabled unless that live handshake is present.

The opt-in prerequisite is now packaged by this flake. Hosts still must run
`kwin-helper-bridge` on the same Plasma session bus, install/load the KWin
package, and use `probe-kwin.sh` to verify the live capability publication and
exact activation/restore transaction before relying on focus-bound input. The
route remains disabled unless that live handshake is present.
