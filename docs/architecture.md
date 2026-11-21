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
The terminal application's `airspy` and `soapy` features enable the corresponding
core backends. Core tests stay in `tests/`; CLI integration tests live in
`apps/terminal/tests/`.

## Configuration

`config::ScanRequest` accepts configuration edits without depending on a UI
toolkit. `resolve()` loads an optional config file and resolves defaults,
selected profile settings, and explicit overrides in that order. It returns
an `EngineConfig`, `RunOptions`, and a preferred output format. Incompatible
scan or device settings across selected profiles return an error.

`ProfilePreset` loads the built-in profiles from the same TOML sources used by
file-based scans. `target_frequencies()` supplies the deduplicated frequency
list to both the scan pipeline and session progress reporting.

`ScanPreferences` provides the profile, gain and recording choices exposed by
the small-screen settings page. It applies changes and creates a `ScanRequest`
through the same resolver. A frontend can offer a different editor without
implementing validation or profile inheritance itself.

`SettingsStore` owns settings paths, JSON loading and atomic replacement.
Settings use `$XDG_CONFIG_HOME/camfinder/settings.json`, with `~/.config` as the
default base. If this file is absent, the previous
`camfinder-cardputer-zero/settings.json` is read. Saving writes the shared path
and leaves the legacy file intact. New recordings use
`$XDG_DATA_HOME/camfinder/recordings`, with `~/.local/share` as the default base.
CLI config and profile files remain supported through the request resolver.

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
tests compare subprocess output with the engine configured through shared
preferences.
