# Camfinder for Cardputer Zero

The device application uses the shared Camfinder scan engine with an Airspy
receiver. It displays harmonic candidates on the built-in 320×170 screen.

## Build and install

On an ARM64 Debian system with Rust installed, install the build dependencies:

```sh
sudo apt-get install build-essential pkg-config libsdl2-dev libsdl2-ttf-dev fonts-dejavu-core
```

From the repository root:

```sh
cargo xtask cardputer-zero
sudo apt install ./target/dist/camfinder-cardputer-zero_0.1.1-1_arm64.deb
```

The package installs an entry in the Cardputer Zero launcher that explicitly
selects the built-in framebuffer, including when the launcher inherits desktop
display variables. The application runs as the logged-in user; this user needs
access to the framebuffer, keyboard and Airspy through the `video`, `input` and
`plugdev` groups. The package includes
an Airspy udev rule and declares its SDL2, SDL2_ttf and font dependencies.

The build target defaults to `aarch64-unknown-linux-gnu`. Cross compilation
requires an ARM64 linker and ARM64 SDL2/SDL2_ttf libraries. Set
`CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER` and target-specific Rust linker
flags for the toolchain in use. `cargo xtask cardputer-zero --help` lists options
for packaging an existing binary or supplying a screenshot.

Packages and review metadata are written to `dist/` under Cargo's target
directory, normally `target/dist/`. This follows `CARGO_TARGET_DIR` or
`build.target-dir`; `--output` overrides the package destination. The GitHub
workflow builds an ARM64 package for version tags and manual runs.

## Controls

| Key | Action |
|---|---|
| `4` | Scan results |
| `5` | Selected candidate details |
| `6` | Profile, gain and recording settings |
| `7` | Capture and processing statistics |
| `8` / Space | Pause, resume or retry after an error |
| Arrows / `F Z X C` | Navigate candidates or settings |
| Enter | Open details, or save settings and restart scanning |
| Esc | Exit to the launcher |

The scan header shows completed/total frequency points and the last pass
duration. `Found current/total` counts entries in the candidate list, including
tracked and temporarily absent candidates. Six entries fit on screen; a scroll
bar indicates the remaining list. Candidates are signal matches, not confirmed
cameras.

Configuration and scan control use the shared core described in
[Core and frontends](../../docs/architecture.md). Settings are saved under
`~/.config/camfinder/`. When no shared settings exist, the previous
`~/.config/camfinder-cardputer-zero/settings.json` is loaded; saving writes the
shared location. Existing recordings stay in their original directory, while
new recordings use `~/.local/share/camfinder/recordings/`. `XDG_CONFIG_HOME` and
`XDG_DATA_HOME` override the base directories. Recording modes are Peaks,
Summary and Spectrum. Full-spectrum recording uses substantially more disk space.

## Preview without hardware

With the native SDL2 development libraries installed:

```sh
cargo run --package camfinder-cardputer-zero -- --demo
```

Generate a 320×170 preview on a desktop or headless build host:

```sh
SDL_VIDEODRIVER=dummy cargo run --package camfinder-cardputer-zero -- \
  --demo --duration 2 --screenshot target/camfinder-preview.png
```

The source package includes the store metadata, icon, screenshot and desktop
entry in `packaging/`. Runtime device errors appear on the scan page; reconnect
the receiver and press `8` to retry.
