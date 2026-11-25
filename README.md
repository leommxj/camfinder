# camfinder

Experimental SDR-assisted camera leakage detector.

The detector looks for weak electromagnetic leakage near crystal-clock
harmonics. It scans configured harmonic frequencies, finds narrow peaks, fits a
shared offset model, and tracks persistent candidates across scans.

**Practical detection relies heavily on an external low-noise amplifier (LNA).**
The [original experiments](https://www.leommxj.com/2026/06/11/EM-leakage-based-hidden-camera-detection/)
used a 20 dB external LNA; neither an Airspy Mini nor a USRP B210 captured the
leakage effectively without one.

The Cargo workspace contains the portable library at the repository root,
the terminal application in `apps/terminal/` for CLI/TUI output, and the
[Cardputer Zero application](apps/cardputer-zero/README.md).
The terminal application builds the `camfinder` command and is selected by
`cargo run` from the repository root.

Configuration and scan sessions are shared across frontends. See
[Core and frontends](docs/architecture.md) for the interfaces and responsibilities.

## Human Says
see [this](https://www.leommxj.com/2026/06/11/EM-leakage-based-hidden-camera-detection/)

## Releases

Pushing a version tag such as `v0.1.1` builds and publishes a
[GitHub Release](https://github.com/leommxj/camfinder/releases) with:

- Linux x86_64 and ARM64 CLI/TUI archives (`camfinder-v<version>-linux-<arch>.tar.gz`).
- A Cardputer Zero ARM64 Debian package (`camfinder-cardputer-zero_<version>-1_arm64.deb`).
- `SHA256SUMS` for the three packages.

The tag must match the version in every workspace package. The release is
published after both build jobs succeed. Ordinary branch pushes do not trigger
the workflow; it builds and packages without running tests.

Terminal archives contain the `camfinder` executable, documentation, licenses,
example configuration, profiles and an Airspy udev rule. Extract the archive and run
`./camfinder scan --preset 24+27`. These builds use the default Airspy backend;
SoapySDR support requires building from source with `--features soapy`. Linux
packages are built on Ubuntu 24.04 and require a compatible glibc-based system.

If the Airspy USB device is not accessible to your user, install the included
`60-camfinder-airspy.rules` into `/etc/udev/rules.d/`, reload the udev rules and
reconnect the receiver. Your user must belong to the `plugdev` group. For the
device application, follow the [Cardputer Zero installation instructions](apps/cardputer-zero/README.md).

## Current Workflow

Run the current 24/27 MHz high-band Airspy profile:

```bash
cargo run -- scan --profile profiles/best_high_airspy.toml
```

Use an embedded profile without depending on the working directory:

```bash
cargo run -- scan --preset 24+27
```

Other presets are `25`, `all`, `24`, and `27`.

Run the 25 MHz high-band profile:

```bash
cargo run -- scan --profile profiles/25mhz_high_airspy.toml
```

Record a wide 24/27 MHz sweep for offline analysis:

```bash
cargo run -- scan --profile profiles/wide_24_27_216_972_airspy.toml \
  --record captures/scene.jsonl --record-mode spectrum --duration 2m
```

Use a one-off frequency list without a profile file:

```bash
cargo run -- scan --profile-freqs 800M,825M,850M,875M,900M \
  --offset-model ppm --min-hits 3 --min-score 20
```

Use plain output instead of the TUI:

```bash
cargo run -- scan --profile profiles/best_high_airspy.toml --once --output table
```

## Profiles

Profiles are TOML files passed with `--profile <path>`. A profile can define
one or more harmonic groups plus scan and device settings. Grouped profiles are
expanded internally, for example `best_high_airspy:24mhz` and
`best_high_airspy:27mhz`.

Important profiles:

- `profiles/best_high_airspy.toml`: current 24/27 MHz high-band Airspy profile.
- `profiles/25mhz_high_airspy.toml`: 25 MHz high-band Airspy profile.
- `profiles/wide_24_27_216_972_airspy.toml`: wide recording profile for
  offline analysis.
- `profiles/test1_soapy_airspy.toml`: SoapySDR Airspy test profile.
- `profiles/test1_soapy_b210.toml`: SoapySDR USRP B210 test profile.

Profiles use `offset_model = "ppm"` by default for the current field-tested
profiles. Override temporarily with `--offset-model fixed_hz|ppm|auto`.

## TUI

The default output is TUI when stdout is interactive.

Controls:

- `Tab` / `Shift-Tab`: switch Suspects, Peaks, and Logs panes.
- `Up` / `Down`: move selection or scroll the focused pane.
- `PageUp` / `PageDown`: page movement.
- `Enter`, `Space`, or `e`: expand/collapse the selected suspect.
- `h`: hide selected suspect.
- `t`: hide/show tracking suspects.
- `u`: restore manually hidden suspects.
- `p`: pause, resume, or retry a failed scan through the shared session.
- `q` or `Esc`: stop.

Suspects keep a stable display order and show the harmonic group, offset, SNR,
score, and a score-derived strength bar.

## Recording

Each JSONL line is one scan pass. Recorded passes include:

- active scan and device settings
- detected peaks and peak shape fields
- candidate results
- persistence summaries
- capture and processing statistics

The default recording mode is `peaks`. Use `--record-mode summary` to add
search-window occupancy metrics, or `--record-mode spectrum` to also store
the search-window spectrum in `spectrum_bands`.

Use `--record-band-decimation N` to store every Nth FFT bin when captures become
too large.

Analyze a saved peak list with the peaks-file backend:

```bash
cargo run -- scan --backend peaks-file --peaks-file peaks.json \
  --profile-freqs 800M,825M,850M,875M,900M
```

## Hardware Backends

List devices:

```bash
cargo run -- devices
```

Airspy native backend:

```bash
cargo run -- scan --profile profiles/best_high_airspy.toml \
  --backend airspy --gain-mode manual --lna 14 --mixer 15 --vga 2
```

SoapySDR backend (on Debian/Ubuntu, install `pkg-config` and
`libsoapysdr-dev` before building):

```bash
cargo build --features soapy
cargo run --features soapy -- scan --profile profiles/test1_soapy_airspy.toml
```

SoapySDR requires the native runtime and driver module, such as
`soapysdr-module-rtlsdr`, `soapysdr-module-hackrf`, or `soapysdr-module-uhd` on
Linux. On Windows, install PothosSDR and add its `bin` directory to `PATH`.

## Config

The terminal and Cardputer Zero applications use the same TOML configuration,
loaded and saved by the core. Both read `~/.config/camfinder/config.toml` by
default (`$XDG_CONFIG_HOME/camfinder/config.toml` when set). If it is absent,
the core uses the embedded defaults.

An embedded `preset = "24+27"` selects detection frequencies and thresholds;
receiver and scan settings come from the same TOML file. Other presets are
`25`, `all`, `24` and `27`. For custom profiles, omit `preset` and use
`profile_files = ["profile.toml"]` or inline `[[profiles]]` entries. Paths in a
configuration or profile file are relative to that file.

Omitted device and scan fields use the embedded defaults. For either receiver,
setting `dwell_ms` without `fft_frames` selects capture by duration; an explicit
`fft_frames` selects a fixed number of FFT frames instead.

Enable recording with `[scan] record = true` and `record_mode = "peaks"`,
`"summary"` or `"spectrum"`. The core creates a new recording under
`~/.local/share/camfinder/recordings/`; `XDG_DATA_HOME` overrides the base.
An explicit `record_path` selects a fixed output file instead.

Print the embedded default config:

```bash
cargo run -- default-config
```

Run with an explicit config:

```bash
cargo run -- --config camfinder.toml scan --profile profiles/best_high_airspy.toml
```

Show the loaded config:

```bash
cargo run -- show-config
```

The default backend is Airspy. Backends that require an input file, such as
`peaks-file` or `iq-file`, report an error if its path is missing.
