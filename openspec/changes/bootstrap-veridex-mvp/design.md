# Design — Veridex v0.1 MVP

## Architecture overview

```
                    ┌─────────────────────────────────────────────┐
   source dataset   │                 veridex-core (Rust)          │
  (LeRobot v3 |     │                                              │
   MCAP)            │   adapters ──► Canonical Dataset Model (CDM) │
        │           │                        │                     │
        └──────────►│                        ▼                     │
                    │   validation-engine ◄─ checks-catalog        │
                    │        │                                     │
                    │        ▼                                     │
                    │   verdict ──► reporting (terminal/JSON)      │
                    │        │                                     │
                    │        ├──► provenance (extract → Croissant) │
                    │        │                                     │
                    │        ▼                                     │
                    │   trust-certificate (score → COSE/JWS sign)  │
                    └───────────────┬──────────────────────────────┘
                                    │ pyo3 / maturin
                     ┌──────────────┴───────────────┐
                     ▼                               ▼
              veridex (CLI binary)      veridex-data (PyPI) → import veridex
```

## Key decisions

### D1 — Rust core with Python bindings
The core is Rust (`veridex-core`) for streaming throughput over 100M+ points and to reuse
Invariant's Rust attestation code. Python bindings via pyo3/maturin ship as `pip install veridex-data`
(distribution name; import module and CLI stay `veridex`) because the ecosystem is Python-first. The CLI is a thin Rust binary over the core. **Verdicts must
be identical** across CLI and Python — both call the same core; bindings add no logic.

### D2 — The CDM is the contract
Every adapter's only job is to populate the CDM faithfully; every check reads only the CDM. Adding
a format later is "write an adapter," never "touch the engine or checks." The CDM canonicalizes to
a stable byte form for content hashing (see D5).

CDM (MVP subset):
- `Dataset { id, metadata, provenance, episodes[] }`
- `Episode { index, start_ts, end_ts, streams[], task, labels[] }`
- `Stream { name, modality, declared_rate, clock_id, frames[] }`
- `Frame { ts, value_ref }` (value_ref points into streamed storage; not all values held in memory)
- `Provenance { scope, elements[] with known|asserted|unknown }`

### D3 — Two adapters at launch, chosen to prove neutrality
LeRobot v3 (the beachhead's format) and MCAP (the cross-domain container, ROS 2 / NVIDIA Isaac
default). Shipping exactly these two forces the CDM abstraction to be real, not a LeRobot wrapper —
and lets us demonstrate "same logical dataset, two formats, equivalent verdict," which is the whole
pitch.

### D4 — Cross-stream synchronization is the headline check
The differentiator is `TEMPORAL.CLOCK_SKEW`: for streams sharing an episode, align on the common
timeline using per-stream `clock_id` and declared latency, and flag drift beyond tolerance. This is
the failure mode that silently corrupts training and that a LeRobot-only linter cannot frame as a
cross-format guarantee. It anchors the demo.

### D5 — Determinism and content hashing
Canonicalize the CDM (stable field order, normalized numeric encodings, sorted streams/episodes)
and hash it (e.g. SHA-256). The verdict and certificate cite this hash. Same bytes + same version
⇒ same hash ⇒ same certificate content hash. Parallelism must not affect finding order (stable sort
by check ID, then CDM location).

### D6 — Certificate signing reuses Invariant
Reuse Invariant's COSE/JWS signing and append-only audit approach rather than reinventing crypto.
The certificate binds to the CDM content hash and verifies offline against a public key. Timestamps
are **caller-supplied** (no wall-clock in core) to keep signing reproducible and testable.

### D7 — Scoring rubric is versioned and boring
v1 rubric: map findings (by severity/category) and provenance coverage to a 0–100 score and A–F
grade via a documented, deterministic function. Ship the rubric text with the release; record
`rubric_version` in every certificate. Do not over-engineer weights in v0.1 — correctness and
determinism over cleverness. Cross-dataset score comparison is only valid within a rubric version.

### D8 — Non-mutation is absolute
Veridex reads datasets and writes only its own outputs to caller-specified paths. No repair, no
in-place edits — this is both a design rule and part of the neutral-verifier brand (repair is
Trajlens's lane).

### D9 — Provenance is honest, not fabricated
Extract what the source encodes; classify each element known/asserted/unknown; never infer. Emit
Croissant for portability. Attestation signing exists in minimal form (enough to sign the
certificate); richer producer-attestation UX is a later change.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| CDM leaks LeRobot assumptions, MCAP maps badly | Build both adapters in the same milestone; the "equivalent verdict across formats" test gates the CDM design. |
| Scope creep into repair/labeling | Non-mutation is a hard contract (D8); repair and annotation-generation are explicitly excluded. |
| Reinventing crypto | Reuse Invariant's substrate (D6); no bespoke signing. |
| Scoring bikeshedding stalls the MVP | v1 rubric is deliberately simple and versioned (D7); iterate later without breaking old certs. |
| Trajlens ships MCAP/provenance first | Move fast on the two differentiators; interoperate (read Trajlens output where useful) rather than duplicate. |
