# camfinder

Experimental SDR-assisted camera leakage detector.

The detector looks for weak electromagnetic leakage near crystal-clock
harmonics. It scans configured harmonic frequencies, finds narrow peaks, fits a
shared offset model, and tracks persistent suspects in the TUI.

## Human Says
see [this](https://www.leommxj.com/2026/06/11/EM-leakage-based-hidden-camera-detection/)

## Current Workflow

Run the current 24/27 MHz high-band Airspy profile:

```bash
cargo run -- scan --profile profiles/best_high_airspy.toml
```

Run the 25 MHz high-band profile:

```bash
cargo run -- scan --profile profiles/25mhz_high_airspy.toml
```

Record a wide 24/27 MHz sweep for offline analysis:

```bash
cargo run -- scan --profile profiles/wide_24_27_216_972_airspy.toml \
  --record captures/scene.jsonl --duration 2m
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
- `q` or `Esc`: stop.

Suspects keep a stable display order and show the harmonic group, offset, SNR,
score, and a score-derived strength bar.

## Recording

Each JSONL line is one scan pass. Recorded passes include:

- active scan and device settings
- detected peaks and peak shape fields
- candidate results
- persistence summaries
- `spectrum_bands` with the full search-window spectrum and occupancy metrics

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
  --backend airspy --gain-mode manual --lna 15 --mixer 15 --vga 2
```

SoapySDR backend:

```bash
cargo build --features soapy
cargo run --features soapy -- scan --profile profiles/test1_soapy_airspy.toml
```

SoapySDR requires the native runtime and driver module, such as
`soapysdr-module-rtlsdr`, `soapysdr-module-hackrf`, or `soapysdr-module-uhd` on
Linux. On Windows, install PothosSDR and add its `bin` directory to `PATH`.

## Config

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

If a required backend setting is missing, scan startup returns an error instead
of silently choosing a hardware path.
