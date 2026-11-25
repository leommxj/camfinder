Based on the crates.io rs-spy 0.5.2 source (MIT), by Xavier Olive.
Upstream commit: `1cfb03ca70c57c3089fc731619d5b512a9c5a9a4`, directory
`crates/rs-spy`. `LICENSE` preserves the upstream MIT license.
Camfinder's lockfile selects nusb 0.2.9.

The manifest is standalone and omits upstream examples and their development
dependency. `lib.rs`, `iqconverter.rs` and `error.rs` match the 0.5.2 release.
Local Rust changes are confined to `transport.rs`:

- Atomically detach a kernel driver and claim the interface; preserve an existing userspace owner instead of disconnecting its stream.
- Reset the bulk endpoint with nusb `clear_halt` before restarting RX; report reset failure. This does not replace discarding settling and warmup samples.
- Add `recv_timeout`, using upstream `TryRecv` to distinguish data, timeout and clean end. Receive failures remain errors.
- Add `stop_and_reclaim`, returning the Airspy handle only after the worker has stopped successfully; propagate worker errors and panics.
- Poll cancellation every 20 ms; stop RX, cancel queued USB transfers, and drain their completions before returning the device.
- Bound cancellation draining to two seconds. Control transfers and other cleanup are separate, so this is not a two-second bound on the entire stop operation.
- Propagate control, USB, overflow and stop errors through reclamation; also send the error to the receiver when the queue has space.
- A full raw receive queue terminates the stream and increments the dropped-chunk counter. It never silently joins samples across a dropped block and never blocks shutdown on a full queue.
- Camfinder changes frequency synchronously only after reclamation. Upstream asynchronous tune commands do not acknowledge completion or define the boundary between old and new samples.

Keep upstream's 0.5.1 fix that drops the receiver before joining the worker,
including in `stop_and_reclaim`. The stricter overflow behavior above is still
needed for Camfinder's sample integrity policy. Preserve upstream 0.5.x receive
semantics and gain normalization; IQ conversion has no local changes.
