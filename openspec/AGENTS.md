# Working with these OpenSpec files

This directory follows the [OpenSpec](https://github.com/Fission-AI/OpenSpec) convention for
spec-driven development. Read this before adding or changing specs.

## Layout

```
openspec/
├── project.md                     # product context, tech decisions, conventions
├── AGENTS.md                      # this file
├── specs/                         # ratified, north-star capabilities (the target system)
│   └── <capability>/spec.md
└── changes/                       # proposed / in-progress work (deltas against specs/)
    └── <change-id>/
        ├── proposal.md            # Why / What Changes / Impact
        ├── design.md              # technical decisions for this change
        ├── tasks.md               # ordered implementation checklist
        └── specs/<capability>/spec.md   # the delta: ## ADDED / MODIFIED / REMOVED Requirements
```

## How this repo uses the convention

Veridex is greenfield, so we use `specs/` to hold the **complete ratified vision** of each
capability — the north star we are building toward. `changes/` holds the **active slice** of work
that is being implemented now.

- `specs/` answers: *what should the finished capability do?*
- `changes/<id>/` answers: *what are we building next, why, and how?*

The first change, `bootstrap-veridex-mvp`, carves the v0.1 shippable slice out of the north-star
specs. When a change is implemented and archived, its deltas are considered folded into `specs/`.

## Spec format

Every capability spec uses:

```markdown
## Purpose
<one paragraph: what this capability is for>

## Requirements

### Requirement: <short name>
The system SHALL <behavior, using SHALL/MUST>.

#### Scenario: <name>
- **WHEN** <trigger / precondition>
- **THEN** <observable outcome>
- **AND** <further outcome>
```

Rules:
- Every requirement has at least one scenario.
- Requirements describe **behavior**, not implementation.
- Change deltas group requirements under `## ADDED Requirements`, `## MODIFIED Requirements`, or
  `## REMOVED Requirements`.

## Capabilities in `specs/`

| Capability | Status | What it owns |
|---|---|---|
| `dataset-ingestion` | Core | Cross-format adapters → the Canonical Dataset Model (CDM). The neutrality layer. |
| `validation-engine` | Core | The check framework: registry, severities, deterministic verdicts, incremental runs. |
| `checks-catalog` | Core | The concrete families of checks (structural, temporal-sync, statistical, semantic, video, provenance, duplicate, privacy). |
| `provenance-lineage` | Core | Capturing/verifying lineage; license compatibility; emitting Croissant + W3C PROV. |
| `trust-certificate` | Core | Scoring, grading, and the signed, portable, versioned certificate. |
| `reporting` | Core | Human- and machine-readable output (terminal, JSON, HTML, SARIF); diffing; coverage disclosure. |
| `cli` | Core | Command surface, exit codes, config discovery, CI integration, Python parity. |
| `configuration` | Core | Config files, policy profiles, tolerances/thresholds, precedence. |
| `extensibility` | Core | Plugin SDK: custom checks, adapters, extractors; check-packs; stable API. |
| `security` | Core | Key management, signing trust model, guarantees/non-guarantees, threat model, audit. |
| `autonomy-sensor-data` | **Core — priority after core** | Multi-sensor rig (LiDAR/radar/camera/CAN/GNSS/IMU/ego-pose): AV-native adapters (MF4, CAN+DBC, ROS bag), rig sync + calibration + ego-pose + sequence checks, world-model readiness profile. |
| `registry` | **Roadmap** | Hosted certification registry: publish/lookup, public verification, badges, revocation. Post-core. |
