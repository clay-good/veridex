# Tasks

## R1 — the reader

- [x] Detect `#ROSBAG V2.0` and refuse anything else by name.
- [x] Walk the top-level record stream (`header_len`/`header`/`data_len`/`data`), reading the
      op-coded record types the format defines.
- [x] Read connection records into (topic, ROS type), and message records into (conn, time, body).
- [x] Read chunk records: uncompressed and LZ4; disclose `bz2` and any unknown compression as
      unread rather than skipping it in silence.
- [x] Bound every length the file declares, so a corrupt or hostile bag is refused rather than
      allocated for.

## R2 — the CDM

- [x] One stream per topic, modality from the ROS type via the shared classifier.
- [x] One frame per message, on the recorder's clock, fingerprinting the body bytes.
- [x] `recorder` provenance from the bag header where it names one.
- [x] Report mapped, unmapped and unread fields the way every other adapter does.

## R3 — proof

- [x] Round-trip tests over a hand-built bag: topics, types, timestamps, fingerprints.
- [x] A corrupt/truncated/hostile bag errors or yields nothing, and never panics.
- [x] The sweep reaches the new reader (`the_sweep_reaches_every_adapter`).
- [x] `docs/formats.md` and the README say what it reads and what it does not.

## R4 — bodies (follow-up, not this change)

- [ ] Reach the existing typed decoders through a ROS 1 (no-encapsulation) reader.
