# Proposal: a LeRobot CI / Hub quality-and-provenance gate

**Audience:** LeRobot / Hugging Face maintainers.
**Ask:** run Veridex as an optional check on dataset PRs and Hub uploads, surfacing a trust score and
a signed certificate.

## Why

Dataset quality — not architecture or compute — is the binding constraint in physical AI, and today
it is unmeasurable at upload time. Two failure modes are common and silent:

- **Corrupted episode boundaries.** Wrong cumulative episode-length metadata misattributes frames
  across episodes ([lerobot#4143](https://github.com/huggingface/lerobot/issues/4143)). Nothing
  fails; training just degrades.
- **Cross-stream clock skew.** A camera and an arm on different clocks drift apart over an episode,
  so the policy learns to act on stale observations.

Neither is caught by a schema check. Both are caught by Veridex, deterministically, with a stated
training risk and a remedy.

## What the gate does

On a dataset PR or upload, run:

```sh
veridex check <dataset> --json
```

- Exit `0` (clean) / `10` (warnings) / `20` (errors) — CI can gate on the threshold it chooses
  (`--fail-on warning` to be strict).
- Emits a versioned JSON report (`veridex.report/1`) with every finding located to the exact
  episode / stream / frame range, plus a 0–100 trust score and A–F grade.
- Optionally `veridex certify` issues a signed, content-bound certificate the Hub can display as a
  badge; it verifies offline, so no Veridex service is in the trust path.

## Why Veridex specifically

- **Cross-format and neutral.** It reads LeRobot v3 *and* MCAP into one Canonical Dataset Model and
  runs the same checks over both — the same dataset in two formats yields an equivalent verdict.
  A neutral verifier is a position no format owner can credibly hold.
- **Provenance, not just structure.** The certificate separates what is *known*, *asserted*, and
  *unknown* (sensor, clock, calibration, annotator, license, upstream), so a clean data score can
  never mask missing lineage.
- **Deterministic and offline.** Same bytes + same version ⇒ same verdict and same certificate
  content hash. Certificates verify against a public key with no network dependency.
- **Non-mutating.** Veridex only reads, reports, and certifies. Repair stays out of scope.

## Rollout

1. **Advisory.** Add a non-blocking CI job that posts the report and score as a PR comment.
2. **Warn-gate.** Block on `error` findings (corrupted boundaries, clock skew); warn on the rest.
3. **Badge.** Display the certificate grade + provenance coverage on the Hub dataset card.

Each step is independently reversible and adds no runtime dependency on Veridex — the certificate is
self-verifying.

## Status

Veridex v0.1 implements the checks and certificate this proposal relies on (LeRobot v3 + MCAP
adapters, structural/temporal/provenance checks including `TEMPORAL.CLOCK_SKEW`, the v1 trust
rubric, and Ed25519-signed offline-verifiable certificates). See the repository README for a
runnable quickstart.
