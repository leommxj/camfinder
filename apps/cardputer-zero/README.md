# Camfinder for Cardputer Zero

Experimental SDR-assisted camera leakage detector for Cardputer Zero.

Camfinder looks for weak electromagnetic leakage near crystal-clock harmonics.
It scans configured harmonic frequencies, finds narrow peaks, fits a shared
offset model, and tracks persistent candidates on the built-in 320×170 screen.

**Practical detection relies heavily on an external low-noise amplifier (LNA).**
The [original experiments](https://www.leommxj.com/2026/06/11/EM-leakage-based-hidden-camera-detection/)
used a 20 dB external LNA; neither an Airspy Mini nor a USRP B210 captured the
leakage effectively without one.

The default package includes the Airspy backend. Both applications can also
use other SDR receivers through the optional SoapySDR backend.

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
`plugdev` groups. The package installs the Airspy udev rule at
`/usr/lib/udev/rules.d/60-camfinder-cardputer-zero.rules` and declares its SDL2,
SDL2_ttf and font dependencies.

The build target defaults to `aarch64-unknown-linux-gnu`. Cross compilation
requires an ARM64 linker and ARM64 SDL2/SDL2_ttf libraries. Set
`CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER` and target-specific Rust linker
flags for the toolchain in use. `cargo xtask cardputer-zero --help` lists options
for packaging an existing binary or supplying a screenshot.

Packages and review metadata are written to `dist/` under Cargo's target
directory, normally `target/dist/`. This follows `CARGO_TARGET_DIR` or
`build.target-dir`; `--output` overrides the package destination. Pushing a
matching version tag such as `v0.1.1` builds the ARM64 package and publishes it
alongside the terminal application in
[GitHub Releases](https://github.com/leommxj/camfinder/releases).

To build with SoapySDR, install `libsoapysdr-dev` and the receiver's SoapySDR
driver module, then run:

```sh
cargo build --locked --release --package camfinder-cardputer-zero --features soapy
```

For an ARM64 package, add `--target aarch64-unknown-linux-gnu` and package that
binary with `cargo xtask cardputer-zero --no-build`. The target system also
needs the receiver's SoapySDR driver module.

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
[Core and frontends](../../docs/architecture.md). Both applications read
`~/.config/camfinder/config.toml`; the settings page saves changes there.
Pass `--config path/to/config.toml` to
edit another configuration, also accepted by the terminal application.
The backend, device, gain and capture settings come from that TOML file.
For SoapySDR, set `[device] backend = "soapy"` and configure `[device.soapy]`.

The Debian package includes `camfinder.default.toml` and the complete `profiles/`
directory under `/usr/share/camfinder-cardputer-zero/`. To customize them, copy
the template and `profiles/` into your configuration directory and rename the
template to `config.toml`. To use a custom profile, replace `preset` with
`profile_files = ["profiles/best_high_airspy.toml"]`; relative paths are resolved
from the configuration file's directory. Package upgrades update the examples
without changing your user configuration.

The settings page edits the detection preset, gain and recording mode.
Changing a preset preserves the receiver settings. Editing Soapy gain selects
manual overall gain; per-stage gains and AGC can be set in the TOML file.
Saving expands referenced profiles into the configuration without modifying
the original profile files.

If the default TOML file is absent, the core uses the embedded defaults.
An explicit `--config` path must exist. Recordings use
`~/.local/share/camfinder/recordings/`. `XDG_CONFIG_HOME` and `XDG_DATA_HOME`
override the base directories. Recording modes are Peaks, Summary and
Spectrum. Full-spectrum recording uses substantially more disk space.

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
