# Agent notes

## UI testing must stay inside the caged session

Never launch the Bamdad GUI, `grim`, `wtype`, or `ydotool` against the user's
live Wayland/niri session for testing. Do not focus, move, click, or type into
live windows. `ydotool` injects input globally through `/dev/uinput`, so a
mistake can click or drag real applications on the working desktop.

All UI runs, screenshots, resizes, and interaction checks must use the isolated
stack in `scripts/caged-preview.sh`:

```
cage (headless wlroots) -> nested niri -> Bamdad
```

Typical use:

```bash
# Build, then start an isolated session (throwaway data dir by default)
cargo build -p bamdad-app
./scripts/caged-preview.sh start

# Capture desktop and phone-sized screenshots
./scripts/caged-preview.sh shot /tmp/bamdad.png 1100
./scripts/caged-preview.sh shot /tmp/bamdad-phone.png 380

# Stop the nested stack when done
./scripts/caged-preview.sh stop
```

Rules:

- The cage uses its own `XDG_RUNTIME_DIR`, `XDG_DATA_HOME`, and niri socket.
  Real profiles are never touched unless `BAMDAD_CAGE_DATA` is explicitly set.
- To test a specific screen or state, prefer a `BAMDAD_PREVIEW_*` environment
  hook (for example `BAMDAD_PREVIEW_PAGE`) or a unit test. If a state is not
  reachable that way, add a preview hook; do not inject input into the live
  desktop.
- Previous caged runs can leave a stale cage/niri process. `start` calls
  `stop` first; if niri reports `NoCompositor`, run `stop`, remove the
  `BAMDAD_CAGE_DIR`, and start again. Check `$BAMDAD_CAGE_DIR/session.log`.
- Use `./scripts/caged-preview.sh windows` to inspect the nested windows, and
  `./scripts/caged-preview.sh resize WIDTH` to resize.
- Screenshots taken by the script are cropped to the Bamdad tile. The cage may
  draw its own title bar; this is expected and is not part of the app.

The goal is simple: the user's current session is read-only. All GUI
experimentation happens in the cage.
