# Core and frontends

The `camfinder` library owns configuration and scanning behavior. The terminal
frontend and Cardputer Zero application translate input into core requests and
render the returned state. SDL2, Ratatui and Clap are frontend dependencies.

| Package | Directory | Responsibility |
|---|---|---|
| `camfinder` | `src/` | Core library and hardware backends |
| `camfinder-terminal` | `apps/terminal/` | CLI arguments, TUI, table and JSON output; builds the `camfinder` binary |
| `camfinder-cardputer-zero` | `apps/cardputer-zero/` | Small-screen display and device input |
| `xtask` | `xtask/` | Build and packaging tools |

The default workspace members are the core and terminal application, so root
`cargo run` launches the terminal application and `cargo test` covers both.
Use `cargo test --workspace` to include the device application and build tools.
Both applications' `airspy` and `soapy` features enable the corresponding
core backends. Core tests stay in `tests/`; CLI integration tests live in
`apps/terminal/tests/`.

## Configuration

`AppConfig` defines the TOML schema used by both applications. It includes
the selected preset or custom profiles, scan settings, device settings and
masks. Its editing methods change those same fields directly.

`config::ScanRequest` accepts temporary overrides without depending on a UI
toolkit. `resolve()` loads an explicit file or the shared configuration, then
applies selected profile settings and explicit overrides. It returns
an `EngineConfig`, `RunOptions`, and a preferred output format. Incompatible
scan or device settings across selected profiles return an error.

`ProfilePreset` loads detection frequencies and thresholds from the existing
profile TOML sources, leaving receiver and scan settings in `AppConfig`.
Custom profile files can still include scan/device overrides. Loading them
merges these into the configuration and expands the profiles inline, so a
subsequent edit and save preserves the effective settings. Relative paths are
resolved against the file containing them. `target_frequencies()` supplies the
deduplicated frequency list to the scan pipeline and progress reporting.

`SettingsStore` owns paths, TOML loading, validation and atomic replacement.
Both applications use `$XDG_CONFIG_HOME/camfinder/config.toml`, with `~/.config`
as the default base, and support an explicit `--config` path. If the default
file is absent, the core uses the embedded defaults. Configuration is stored
only as TOML.

`[scan] record = true` requests a new recording for each session;
`record_path` optionally selects a fixed file. Automatic paths use
`$XDG_DATA_HOME/camfinder/recordings`, with `~/.local/share` as the default base.
The core resolves these paths for both frontends. Capture budgets and default
gain settings live in `camfinder.default.toml` and are not reapplied at startup.

## Running scans

`ScanEngine` owns hardware access, the reusable DSP pipeline, detection,
persistence and recording. It can execute a single pass synchronously for
library users and tests.

`ScanSession` runs the engine in the background. It owns repeat scheduling,
duration limits, pause/resume, retry, applying configuration and shutdown.
Duration limits interrupt capture and also apply while paused or waiting
between passes. Only complete passes update persistence and recordings.

Interactive frontends read `ScanSnapshot` and send session commands. Snapshots
contain progress, timing, candidates and bounded logs; full spectrum data stays
out of the UI snapshot. CLI table/JSON output consumes `observe()` events from
a bounded queue so completed passes are delivered in order. Cancellation also
interrupts a blocked publisher when its consumer stops reading.

`PersistenceSummary::current_result()` associates a tracked candidate with the
nearest compatible current detection. All frontends use this association;
frequency-offset tolerances and tie behavior are independent of rendering.

## Frontend responsibilities

The terminal frontend owns argument syntax, terminal setup, table/JSON
formatting, focus, scrolling and display filters. The Cardputer Zero frontend
owns SDL rendering, framebuffer/input adaptation, page navigation and PNG
screenshots. Neither frontend selects capture budgets, merges profile settings,
tracks candidates or writes scan recordings directly.

The core builds with `cargo check -p camfinder --no-default-features`.
Core configuration and session tests run without a receiver; CLI integration
tests compare subprocess output with the engine using the same saved TOML.
