# Policy profiles

A **profile** is a named bundle of thresholds and pass/fail *criteria*. It does not change which
checks run — the full catalog still runs and scores the dataset — it sets the run's tolerances and
names the subset of checks whose results form a **readiness** verdict, which the certificate reports
per-criterion.

Use one when issuing a certificate:

```sh
veridex certify my-rig.mcap --key issuer.key --profile world-model-ready
```

The certificate gains a signed `readiness` section, and the terminal prints each criterion's result.

## `world-model-ready`

For multi-sensor autonomy rigs. It tightens cross-sensor sync and bundles the four autonomy criteria a
world-model training set needs. A dataset is **ready** only when the profile applies *and* every
criterion's check **ran cleanly and found nothing**.

Two rules keep that honest:

- **Applicability demands the data the criteria are about.** The profile applies to a sensor rig that
  carries a perception sensor (LiDAR or camera) *and* an ego trajectory. A bus-only measurement (a
  CAN or MF4 log) is a rig by sensor count, but calibration completeness and ego-pose continuity have
  nothing to examine there, so it is reported `N/A` rather than passing on empty criteria.
- **Silence is not a pass.** A check that was disabled in `veridex.toml`, filtered out of the run, or
  that failed internally produces no findings — so each criterion records whether its check actually
  ran, and one that didn't blocks `ready` and prints as `? … [check did not run]`. A dataset cannot
  be certified ready by switching the checks off.

| Criterion | Check | Passes when |
|---|---|---|
| Rig sync | `autonomy.rig-sync` | rig sensors within a **20 ms** cross-sensor span drift (stricter than the 50 ms default) |
| Sequence completeness | `autonomy.sequence-complete` | no rig sensor dropping more than 5% of its frames |
| Ego-pose continuity | `autonomy.ego-pose-continuity` | ego trajectory continuous (no step above 100 m/s implied speed) |
| Calibration completeness | `autonomy.calibration-completeness` | connected transform (TF) tree and camera intrinsics present |

The `readiness` block on the certificate records the profile name, whether it was `applicable`, the
overall `ready` flag, and each criterion's `check_id`, `threshold`, `passed`, `ran`, and finding count — and
it is signed like every other field, so a reader can trust it offline. The certificate claims nothing
beyond the criteria listed.

## Reading a readiness certificate back

`verify` reports what the certificate attests, not just that the signature checks out:

```sh
veridex verify my-rig.mcap --certificate my-rig.veridex.json --key issuer.pub
```

```
✓ certificate verified
  issuer key: 8f3c…
  issued at:  1700000000
  dataset:    my-rig
  bound to:   4a1b9c2d7e5f0813…
  trust:      B (82)  [data pass (warnings) · provenance 66%]
  world-model-ready profile: NOT READY
    ✓ autonomy.rig-sync — rig sensors within a 20 ms cross-sensor span drift
    ✗ autonomy.sequence-complete — no rig sensor dropping more than 5% of its frames
    ✓ autonomy.ego-pose-continuity — ego trajectory continuous (no step above 100 m/s implied speed)
    ✓ autonomy.calibration-completeness — connected transform (TF) tree and camera intrinsics present
```

Add `--json` for the machine-readable summary (the same fields, plus the `readiness` block verbatim)
— byte-identical to what `veridex.verify(...)` returns from Python.

Nothing here is re-derived from the dataset: every line comes out of the signed document, so editing
a criterion to read `passed` makes the certificate fail verification instead of printing a nicer
verdict. Verification is fully offline.

From Python:

```python
cert = veridex.certify("my-rig.mcap", secret_key_hex, profile="world-model-ready")
readiness = json.loads(veridex.verify(cert, "my-rig.mcap"))["readiness"]
```
