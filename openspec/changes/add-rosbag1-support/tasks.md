# Tasks

## R1 — the reader

- [ ] Detect `#ROSBAG V2.0` and refuse anything else by name.
- [ ] Walk the top-level record stream (`header_len`/`header`/`data_len`/`data`), reading the
      op-coded record types the format defines.
- [ ] Read connection records into (topic, ROS type), and message records into (conn, time, body).
- [ ] Read chunk records: uncompressed and LZ4; disclose `bz2` and any unknown compression as
      unread rather than skipping it in silence.
- [ ] Bound every length the file declares, so a corrupt or hostile bag is refused rather than
      allocated for.

## R2 — the CDM

- [ ] One stream per topic, modality from the ROS type via the shared classifier.
- [ ] One frame per message, on the recorder's clock, fingerprinting the body bytes.
- [ ] `recorder` provenance from the bag header where it names one.
- [ ] Report mapped, unmapped and unread fields the way every other adapter does.

## R3 — proof

- [ ] Round-trip tests over a hand-built bag: topics, types, timestamps, fingerprints.
- [ ] A corrupt/truncated/hostile bag errors or yields nothing, and never panics.
- [ ] The sweep reaches the new reader (`the_sweep_reaches_every_adapter`).
- [ ] `docs/formats.md` and the README say what it reads and what it does not.

## R4 — bodies (follow-up, not this change)

- [ ] Reach the existing typed decoders through a ROS 1 (no-encapsulation) reader.
