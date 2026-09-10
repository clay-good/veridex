# Tasks

## B1 — the reader

- [x] Detect the `LOGG` file header and read its size, so a truncated or non-BLF file is refused by
      name rather than parsed as one.
- [x] Walk the object stream (`LOBJ` signature, header size, object size, object type), bounding
      every declared length against the bytes that actually remain.
- [x] Read `LOG_CONTAINER` objects: uncompressed and zlib, under the run's decompression budget,
      reassembling an object that straddles two containers.
- [x] Read `CAN_MESSAGE` and `CAN_MESSAGE2` into the adapter's existing `CanFrame` (channel, id,
      DLC, payload, timestamp), honouring the header flag that selects 10 µs ticks or nanoseconds.
- [x] Disclose what is not read: undecoded object types, an unknown container compression, and a
      payload shorter than its declared length.

## B4 — CAN-FD

- [x] Read `CAN_FD_MESSAGE` and `CAN_FD_MESSAGE_64`, taking the payload's extent from the object's
      valid-byte count rather than from the DLC, which on an FD bus is a *code* and not a length.
- [x] Read the candump CAN-FD line form (`<id>##<flags><data>`), which every reader before this
      counted as a line that did not parse.
- [x] Decode a little-endian signal from the byte it starts in, so a signal an FD database places
      past bit 63 is not silently absent.
- [x] Prove all three on payloads whose signals live past byte eight, and prove the two FD object
      types reach identical values.

## B2 — the adapter

- [x] `find_inputs` collects `.blf` logs beside the `.dbc`; detection claims a directory whose only
      logs are BLF.
- [x] Both kinds of log merge into one recording, ordered by timestamp, with the channel becoming
      the same per-bus stream key a candump `can0` does.
- [x] The undecoded-log disclosure drops `.blf` now that it is read, and keeps the rest.

## B3 — proof

- [x] Round-trip over a hand-built BLF: ids, payloads, timestamps and channels, uncompressed and
      zlib, including an object split across two containers.
- [x] The same traffic written as candump and as BLF yields the same signals and the same values.
- [x] A truncated, corrupt or hostile BLF errors or yields nothing, and never panics: a prefix sweep
      and a bit-flip sweep, each result a verdict or a reason.
- [x] A zip bomb in a container is refused by the decompression budget rather than allocated for.
- [x] `docs/formats.md`, the README and the CHANGELOG say what a BLF yields and what it does not.
