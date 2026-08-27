# veridex-core

The Rust core of [Veridex](https://github.com/clay-good/veridex) — the open, cross-format trust
layer for physical-AI data.

This crate is the library behind the `veridex` CLI and the `veridex` Python package. It owns:

- the **Canonical Dataset Model** (`cdm`) every adapter populates and every check reads, and its
  deterministic [content hash](https://docs.rs/veridex-core/latest/veridex_core/canonical/) —
  the same dataset bytes and the same Veridex version always hash alike;
- the **adapters** that map LeRobot v3, RLDS/TFDS, HDF5, Zarr, MCAP, ROS 2 rosbag2, CAN+DBC and ASAM MDF/MF4 into
  that one shape;
- the **validation engine** and the [checks catalog](https://github.com/clay-good/veridex/blob/main/docs/checks.md)
  it runs — structural, temporal, statistical, semantic, video, provenance, and autonomy families;
- the **trust score** and the Ed25519-signed **certificate** that makes a verdict portable and
  verifiable offline.

```rust
use veridex_core::{default_registry, run_check, status_label, IngestOptions, Source};

let registry = default_registry();
let source = Source::Local("my-dataset".into());
let out = run_check(&registry, &source, None, &IngestOptions::default())?;
println!(
    "{} ({}) — {}",
    out.trust.score,
    out.trust.grade.letter(),
    status_label(out.verdict.status)
);
# Ok::<(), veridex_core::IngestError>(())
```

(That example is compiled as a doctest on `veridex_core`'s crate docs, so it cannot drift from the
API.)

Everything the CLI does runs through this crate, and both front-ends call one pipeline, so a verdict
is identical whichever surface produced it.

Full documentation, the check catalog, and the quickstart live in the
[repository](https://github.com/clay-good/veridex). MIT licensed.
