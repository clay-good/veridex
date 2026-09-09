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

## R4 — bodies

- [x] Teach the message reader the ROS 1 encoding: no encapsulation header, no alignment padding,
      a `seq` at the front of every `std_msgs/Header`.
- [x] One dispatch from a ROS message type to the CDM (`adapter::rosmsg`), shared by the MCAP
      adapter, both rosbag2 storage plugins and this reader, with the encoding as a parameter.
- [x] Read `header.seq` as the publisher's own count — the one direct evidence of a dropped message
      a recording holds, and a field ROS 2 does not carry.
- [x] Prove it on hand-built ROS 1 bodies: the values decoded, an unpadded field behind an
      odd-length `frame_id`, a CDR body in a bag counted as failed rather than misread, and a hole
      in `seq`.
- [x] `docs/formats.md` and the CHANGELOG say what a bag now yields.
