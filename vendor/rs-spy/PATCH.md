Based on the crates.io rs-spy 0.4.1 source (MIT), by Xavier Olive.
Upstream commit: `5722503f82d56fcdafaaca1c2bee4eccb6f123cb`.
`LICENSE` preserves the repository license at that commit (`license.md`).

Local transport changes for frequency scanning:

- Atomically detach a kernel driver and claim the interface; preserve an existing userspace owner instead of disconnecting its stream.
- Reset the bulk endpoint with nusb `clear_halt` before restarting RX; report reset failure.
- Add timed receives and `stop_and_reclaim`, returning the stopped Airspy handle.
- Poll cancellation every 20 ms; stop RX, cancel queued USB transfers, and drain their completions before returning the device.
- Bound cancellation draining to two seconds and propagate control, USB, overflow, and stop errors to the caller through reclamation.
- A full raw receive queue terminates the stream and increments the dropped-chunk counter. It never silently joins samples across a dropped block and never blocks shutdown on a full queue.
- Preserve the existing receiver/control API. Camfinder changes frequency synchronously only after reclamation, rather than using the legacy asynchronous tune command.
