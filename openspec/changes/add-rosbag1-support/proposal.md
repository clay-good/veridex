# Add ROS 1 rosbag (`.bag`) support

## Why

ROS 1 is still where a large share of the world's robot data sits. Every lab that recorded before
ROS 2, every fleet that has not migrated, and every public dataset shipped as `.bag` is currently a
dataset Veridex cannot open at all — it is refused at ingest, with no report and no score.

That is the one failure mode a cross-format verifier cannot have. Veridex's claim is that *which
format a team chose does not change whether their data can be checked*; a whole generation of
recordings sitting outside that claim is the largest remaining hole in it. The adapter is also
cheap relative to its reach: a `.bag` is a record stream with the same shape as the containers
already read, and every check, score, certificate and emit path downstream is format-neutral by
construction.

## What changes

A ninth adapter, `rosbag1`, reading ROS 1 bag format v2.0:

- **Detection** by the `#ROSBAG V2.0` magic, so a `.bag` is recognized by content rather than by
  extension.
- **Topics to streams, messages to frames.** Connection records name each topic and its ROS type;
  the type feeds the existing modality classifier, so a `.bag` rig types the same way an MCAP one
  does. Message records supply the recorder's clock and the body bytes to fingerprint.
- **Chunk storage.** Uncompressed and LZ4 chunks are read. `bz2` needs a decompressor this
  workspace does not carry, and is **disclosed as unread** rather than guessed at — the same
  treatment an MF4 `##DZ` in an unknown zip type gets.
- **Bodies, in a second step.** ROS 1 puts the same fields in the same order as CDR, but with no
  encapsulation header, no alignment padding and a `seq` at the front of every `std_msgs/Header`, so
  the existing typed decoders can be reached from it through a reader that knows those. That is
  deliberately *not* in this change: a bag whose topics, types, clock and fingerprints are read
  already reaches the structural, temporal, semantic and provenance families, and shipping that
  first keeps the change reviewable.

## Impact

- New: `adapter/rosbag1.rs`, registered in the default registry.
- `docs/formats.md` gains a ROS 1 section; the README's adapter list and format count grow by one.
- No CDM change, no `CANONICAL_VERSION` bump, no new check: this is a reader, and everything it
  produces is shapes the CDM already has.
