# TextureCraft iPadOS runner

This crate is the Rust static library the Xcode harness in `packaging/ios` links into the
iPad app bundle. `apple_entry` exports Apple's `main`, `scenes` registers the single window
scene iPadOS 27 requires and hands winit's own window to it, `files` presents the native
document pickers, and `pencil` supplies the Apple Pencil input UIKit knows but egui cannot
infer. Everything else — Metal through eframe/wgpu, egui, the whole editor — is the same
Rust the desktop and browser builds run. `unsafe` is denied crate-wide except in those
audited bridge modules.

The root viewport follows the operating system's size; no simulator or device size is
hard-coded. egui lays the UI out in `content_rect`, which excludes the home-indicator inset,
and `safe_area.rs` feeds UIKit's `safeAreaInsets` into each frame because egui-winit reports
zero until the scene's key window exists. The status bar stays hidden through winit's API so
it cannot overlay the menu bar. Only Metal is allowed as a backend, and `compatible_limits`
keeps egui's two inter-stage shader variables when a simulator's device reports 15.

Logs go to `$HOME/Library/Application Support/ai.storyteller.texturecraft/logs/runner.log`
inside the app container; a missing container or unwritable file is reported and never
blocks launch. Calls to `run()` from a non-Apple host return an error without opening a
window.

```sh
cargo test -p texturecraft-ios --lib
cargo clippy -p texturecraft-ios --target aarch64-apple-ios-sim --lib --no-deps -- -D warnings
cargo build --release -p texturecraft-ios --target aarch64-apple-ios-sim
cargo build --release -p texturecraft-ios --target aarch64-apple-ios
cargo xtask ios --sim
# Physical iPad (development bundle id when the product id cannot be registered):
cargo xtask ios --device --device-id <udid> --team <team id> --bundle-id <dev id>
```

## Touch and Apple Pencil

What the shell does, and what has to be confirmed on real glass. The unit tests in
`crates/ui-egui/src/stylus.rs` cover the pen-identity logic (which contact paints, a palm
that lands first, one read per frame, the double-tap edge) but nothing there can prove the
UIKit ordering; check the boxes below in the simulator with the pointer, then on an iPad
with an Apple Pencil.

3D view (`crates/ui-egui/src/viewport.rs`, `view_3d`):

- [ ] one finger drags to orbit; pinch zooms the camera; two fingers pan — the pointer
      (trackpad/mouse) path still works unchanged
- [ ] a resting palm does not move the camera or paint while the pen is down
- [ ] a finger that touched the glass *before* the pen still leaves the stroke on the pen

2D UV view (`view_2d`):

- [ ] two fingers pan the UV layout; pinch zooms about the gesture's centre
- [ ] the pen paints at its own pressure (a light touch makes a thin line, a firm one thick)
- [ ] a finger paints with full pressure, as before
- [ ] lifting the pen ends the stroke; a palm lifting afterwards does not cut one short
- [ ] holding the pen still does not trigger egui's press-and-hold (no context menu)

Pencil system integration (`apps/texturecraft-ios/src/pencil.rs`):

- [ ] double-tap the pen swaps Paint ⇄ Eraser, and the swap follows Settings › Apple Pencil ›
      Double Tap (a "Don't Allow" choice means no swap)
- [ ] the observer sees each touch before winit's view, so palm rejection is active from the
      first frame of a stroke — if a palm that lands before the pen still orbits the view,
      the ordering assumption in `pencil.rs` is wrong and the pen falls back to egui's
      emulated pointer
- [ ] Paint, Eraser, Smudge and Clone strokes all take pen pressure; Path and Polygon Fill
      behave as taps

Deferred, not missing by accident: hover (no UI consumes it yet), tilt and barrel roll,
multiwindow ownership, and open-in-place from Files (the runner ignores URL contexts, which
is why `LSSupportsOpeningDocumentsInPlace` is off in the plist).
