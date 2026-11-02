# TextureCraft iPad bundle

This Xcode target packages the Rust `texturecraft-ios` static library. It contains no
Swift or Objective-C product source. Open `TextureCraft.xcodeproj` and use the shared
`TextureCraft` scheme. Set your development team in Xcode for physical-device signing.

```sh
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
xcodebuild -project packaging/ios/TextureCraft.xcodeproj -scheme TextureCraft \
  -configuration Debug -destination 'generic/platform=iOS Simulator' build
python3 -m unittest discover -s packaging/ios/tests -v
python3 packaging/ios/validate-bundle.py path/to/TextureCraft.app
cargo xtask ios --sim
```

`xtask ios --sim` builds, boots, installs, and launches through the simulator CLI.
For a physical iPad pass `--device --device-id <udid> --team <id>`; `--bundle-id <id>` overrides
the bundle identifier for both build and launch when the product id cannot be registered to your
team. It does not require a desktop viewer. Xcode 27 provides Device Hub;
`--show-simulator` currently targets the older Simulator application.

The build phase resolves Cargo from `CARGO` or `$HOME/.cargo/bin` and builds only
`--lib`. It maps arm64 device, arm64 Simulator, and Intel Simulator architectures
and creates a universal archive when Xcode requests multiple architectures.
Debug uses Cargo's debug profile; Release uses `--release`. Artifacts default to
`target/ios-xcode`; `CARGO_TARGET_DIR` can override that location. Paths containing
spaces are supported. Rust targets must be installed before building.

The library must export Apple's ordinary C entry symbol `main`:
`extern "C" fn main(argc: i32, argv: *const *const c_char) -> i32`.
A dedicated link phase invokes Apple's clang driver because Xcode skips its
standard link phase for targets with no C/Swift/Objective-C sources. It links each
requested architecture using Xcode's SDK and deployment target, then creates the
bundle executable with lipo before signing. The linker pulls the Rust entry symbol
from the archive with `-u _main`. Release builds generate a dSYM.
The bundle validator checks that CFBundleExecutable exists, contains a Mach-O
EXECUTE header, and passes strict codesign verification. Use `--unsigned` only for
intentional unsigned builds. The runner owns
UIKit startup and must keep interop within its Apple bridge modules.

The plist permits iPad multitasking in every orientation and declares one window scene,
required by iPadOS 27. Multiple scenes stay disabled until the runner can give each
window its own document state. The document types are the `.tcraft` project the app saves
and `public.image`, which File › Import resources reads; meshes, materials and HDR
environments are picked inside the app because Apple declares no system type for them.
Opening documents in place is off: the runner ignores the scene's URL contexts, so a file
shared from Files would open an empty project rather than itself.
The entitlement file is deliberately empty: iOS already sandboxes applications;
macOS app-sandbox and user-selected-file entitlements do not belong here.

The asset catalog's `AppIcon` is TextureCraft's engraved zebra from `assets/app-icon/`
(attributed in `ATTRIBUTION.md`), rendered without its rounded-corner clip and flattened
onto the full-bleed `#c2672b` copper field with no alpha channel, since iOS applies its own
corner mask and rejects transparency. It is one 1024 pt image that Xcode scales to every
iPad size.

Touch and Apple Pencil behaviour — two-finger pan, pinch zoom, pen pressure, palm
rejection, the pencil double-tap — is verified against the checklist in
`apps/texturecraft-ios/README.md`.
