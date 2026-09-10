# Add Vector BLF (`.blf`) CAN log support

## Why

Veridex reads CAN traffic only as **candump ASCII** — the text format `can-utils` writes on Linux.
Almost no vehicle data is logged that way. CANoe, CANalyzer, CANape and every Vector interface write
**BLF**, and a directory holding a `.blf` beside its `.dbc` is a dataset Veridex refuses at ingest:
no report, no score, nothing to hand anyone. The adapter disclosing it as unread coverage (the
previous change) makes the gap honest; it does not close it.

This is the CAN half of the same argument the ROS 1 bag change made: *which format a team chose must
not change whether their data can be checked*. And it is cheap relative to its reach — a BLF is a
sequence of length-prefixed objects inside zlib containers, the decompressor is already a dependency
(`flate2`), and everything downstream of the frame is the CAN+DBC adapter's existing signal decode,
statistics, provenance and range checks. What lands is a second *reader* in front of machinery that
is already built and tested.

## What changes

The `candbc` adapter reads BLF logs alongside the candump ones it already reads:

- **Detection** by the `LOGG` file magic, so a `.blf` is recognized by content rather than by
  extension, and a directory holding a `.dbc` plus only `.blf` logs is claimed rather than refused.
- **Objects to frames.** `CAN_MESSAGE` and `CAN_MESSAGE2` objects supply channel, id, DLC, payload
  and timestamp — the same `CanFrame` the candump reader produces, so both kinds of log merge into
  one recording and every existing signal decode, statistic and range check applies unchanged.
- **Containers.** `LOG_CONTAINER` objects are uncompressed or zlib-compressed; both are read, under
  the run's existing decompression budget, and an object stream that straddles a container boundary
  is reassembled rather than dropped.
- **What is not read is named.** A CAN-FD object, an object type this reader does not decode, a
  container in an unknown compression, and a payload that does not match its own declared length are
  each **disclosed as unread coverage** rather than skipped in silence — the discipline every other
  reader here follows.
- **Untrusted input.** Every length the file declares is validated against the bytes that remain, the
  walk is iterative, and decompression is charged to the budget before it is allocated for.

## Impact

- `adapter/candbc.rs` gains a BLF reader (its own module) and its detection widens to a directory
  whose logs are BLF.
- `docs/formats.md` and the README say what a BLF yields; the CHANGELOG records the gap it closes.
- No CDM change, no `CANONICAL_VERSION` bump, no new check: this is a reader, and every frame it
  produces is a shape the CDM and the CAN checks already have.
