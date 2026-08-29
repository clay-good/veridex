# Veridex — Project Context

## What Veridex is

Veridex is the open, cross-format **trust layer for physical-AI data**. It verifies that a
robot or sensor dataset is clean, correctly time-synchronized, and traceable to its origin,
then stamps it with a portable, signed **provenance certificate** — so any team can tell in
seconds whether the data they are about to train on will improve their model or silently
poison it.

One sentence for the room: *the neutral layer that tells you, across any format, whether the
data you're about to train on is clean and where it came from.*

## The problem

In physical AI / vision-language-action (VLA) robotics, **data quality — not architecture or
compute — is the binding constraint**, and today it is unmeasurable across formats:

- A curated ~5% coreset can recover 85–90% of full-dataset performance; most volume in current
  corpora does little work, and some actively hurts (measurable negative transfer when pooling
  heterogeneous robot data).
- ~0.3% poisoned episodes can backdoor a policy.
- Datasets arrive in incompatible containers (LeRobot, MCAP, RLDS, HDF5/Zarr) with no shared way
  to check timestamp alignment, episode integrity, or **where any of it came from** — provenance
  is routinely lost.

Teams still train on unvetted data because checking it is manual, per-format, and provenance
simply isn't captured.

## The wedge (why this is defensible)

Every funded player in this space — Hugging Face / LeRobot, Rerun, LanceDB, NVIDIA — is a
**destination**: each wants your data in *their* format and platform. None can credibly be the
neutral verifier *across* formats, because certifying a competitor's format is not in their
interest. Veridex owns the one position an incumbent structurally cannot: **Switzerland**. Its
neutrality is the moat.

The nearest existing tool, **Trajlens**, is LeRobot-only and does no provenance. Veridex's two
differentiators are exactly the flanks it leaves open:

1. **Cross-format.** One validation model over a Canonical Dataset Model (CDM) that MCAP, RLDS,
   LeRobot, HDF5/Zarr all map into.
2. **Provenance / lineage / attestation.** Which sensor, clock, calibration, annotator, license,
   and upstream dataset produced each segment — emitted as a signed, portable certificate
   (Croissant + W3C PROV under the hood).

Veridex **interoperates with** Trajlens and LeRobot rather than duplicating them.

## Scope of these specs

The specs capture the **full vision**. Capabilities are marked by build status:

- **Core (open source, near-term):** ingestion/CDM, validation engine, checks catalog,
  provenance/lineage, trust certificate, reporting, CLI, configuration, extensibility, security.
- **Roadmap (post-core):** the hosted certification **registry** (publish/lookup, public
  verification, badges, revocation) — the natural commercial layer atop the MIT core. It is
  specified so the north star is complete, and is clearly marked `Status: Roadmap`; it gets its own
  change proposal once the core has adoption. Every certificate stays fully verifiable **offline**
  without it.

Only the `bootstrap-veridex-mvp` change is scoped for immediate implementation; everything else is
the ratified target.

## Relationship to Invariant

Veridex is a **separate** repo and product from [Invariant](../invariant) (a runtime
command-validation firewall). Different lifecycle stage (training-time vs. runtime), different
users, different data model. Veridex **follows Invariant's proven pattern** for signed verdicts —
sign canonical bytes, bind to a content hash, verify offline, no wall-clock in core — rather than
inventing one. It does **not** share a crate: the two repos ship separately and their payloads are
unrelated, so Veridex mirrors the pattern in its own module and shares only the underlying
`ed25519-dalek` dependency. Invariant wraps its attestations in COSE (`coset`); Veridex does not.
The decision and its consequences are recorded as design D6a.

## Technical decisions

| Decision | Choice | Rationale |
|---|---|---|
| Core language | **Rust** (`veridex-core`) | Streams 100M+ points; must be fast and memory-safe. Lets us reuse Invariant's Rust attestation substrate. Differentiates on speed vs. the Python-based incumbent. |
| Ecosystem bindings | **Python** (`veridex-data` on PyPI, via pyo3/maturin) | The entire robot-data world (LeRobot, HF `datasets`) is Python-first. `pip install veridex-data`, `import veridex`. The PyPI *distribution* name is `veridex-data` (bare `veridex` is taken); the import module and CLI stay `veridex`. |
| CLI | single binary `veridex` | `veridex check`, `veridex certify`, `veridex verify`, `veridex provenance`, `veridex inspect`. |
| Internal model | **Canonical Dataset Model (CDM)** | The neutrality substrate: every format adapter maps into it; every check and the certificate operate on it. See `design.md` in the bootstrap change. |
| Certificate | signed JSON (detached **Ed25519** over domain-separated canonical bytes), `<dataset>.veridex.json` | Portable, offline-verifiable, cites the CDM content hashes. Mirrors Invariant's signed-verdict pattern without a COSE/JOSE stack, so verification is reimplementable anywhere (design D6a). |
| Metadata emit | **MLCommons Croissant** + **W3C PROV** | Ride existing standards for distribution; do not invent a rival metadata format. |
| License | **MIT** | Ubiquity for the open core; trust infrastructure is the eventual commercial layer. |

## Conventions

- Specs are behavioral (what, not how). Language/architecture choices live in `project.md` and
  each change's `design.md`, not in capability specs.
- Checks are **deterministic**: the same dataset bytes and the same Veridex version always
  produce the same verdict and the same certificate content hash. Reproducibility is a
  first-class requirement.
- Veridex **never mutates a user's dataset**. It reads, verifies, reports, and certifies. Repair
  is out of scope (Trajlens does repair; we stay a neutral verifier).
- Cross-format neutrality is load-bearing: no capability may assume a single source format.
- Provenance is best-effort on extraction (capture whatever the source encodes) and explicit on
  attestation (let the producer sign the rest); a certificate always states what is *known*,
  *asserted*, and *unknown*.

## Glossary

| Term | Meaning |
|---|---|
| **CDM** | Canonical Dataset Model — Veridex's single internal representation that all adapters populate and all checks read. The neutrality substrate. |
| **Adapter** | A per-format component that maps a source (LeRobot, MCAP, RLDS, HDF5/Zarr) into the CDM. |
| **Check** | A deterministic rule that inspects the CDM and emits findings. Grouped into categories (structural, temporal, statistical, semantic, video, provenance, autonomy). |
| **Finding** | A single issue: check ID, severity, precise CDM location, message, code, risk, remedy. |
| **Verdict** | The full, deterministic result of a run: findings, status, and reproducibility metadata. |
| **Trust score / grade** | A 0–100 score and A–F grade computed from the verdict + provenance coverage by a versioned rubric. |
| **Certificate** | A portable, signed, content-bound statement about a dataset. A "nutrition label," not a seal of approval. Offline-verifiable. |
| **Provenance class** | Each provenance element is `known` (extracted), `asserted` (producer-attested), or `unknown` (absent). |
| **Rubric version** | The versioned scoring function; scores only compare within the same rubric version. |
| **Check-pack** | A distributable, versioned bundle of third-party checks. |

## Roadmap (direction, not commitment)

1. **v0.1 MVP** — `bootstrap-veridex-mvp`: LeRobot (v2.0/2.1/3.0) + MCAP adapters, structural/temporal(cross-stream
   skew)/statistical/provenance checks, Croissant emit, signed certificate, CLI + Python parity.
2. **Autonomy / world-model sensor data (priority once core is built and tested)** —
   `add-autonomy-support`: multi-sensor rig CDM extensions, AV-native adapters (ASAM MDF/MF4, CAN+DBC,
   ROS bag), rig-wide sync + calibration + ego-pose + sequence-completeness checks, autonomy
   provenance, and a world-model readiness profile. The first major post-core expansion.
3. **Format breadth** — RLDS/TFDS, HDF5, Zarr adapters; the "same dataset, many formats, one verdict"
   proof.
4. **Depth** — semantic + video deep checks; language-annotation verification; duplicate/PII checks;
   HTML/SARIF reporting; verdict diffing; `watch` mode.
5. **Ecosystem** — plugin SDK + check-packs; policy profiles; config maturity.
6. **Registry (commercial layer)** — hosted publish/lookup, public verification, badges, revocation.
7. **Adoption / anointer** — upstream proposal to make Veridex a LeRobot CI/Hub quality-and-provenance
   gate (closes the `lerobot#4143` class); benchmark/dataset-track submission gates.

## Repository and licensing

- **License:** MIT (`LICENSE` at repo root). The open core is MIT; the eventual commercial layer is
  the hosted registry/service, not a license change.
- **Repository:** GitHub, `clay-good/veridex`, with CI on every push (the README carries the badge).
  A `.gitignore` at the root
  excludes build artifacts, Python/maturin outputs, secrets and signing keys (`*.key`, `*.pem`), and
  locally generated Veridex outputs (`*.veridex.json`, `reports/`). Do not commit keys or generated
  certificates.
- **Names (resolved):** GitHub repo and crates.io crates use **`veridex`** (`veridex`, `veridex-core`,
  `veridex-cli`). The CLI binary and Python import module are **`veridex`**. The PyPI distribution is
  **`veridex-data`** because the bare `veridex` PyPI name is already taken.
