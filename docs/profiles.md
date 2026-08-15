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
world-model training set needs. A dataset is **ready** only when it is actually a sensor rig *and*
every criterion's check produced no findings — a non-rig dataset is reported `N/A`, never a vacuous
pass.

| Criterion | Check | Passes when |
|---|---|---|
| Rig sync | `autonomy.rig-sync` | rig sensors within a **20 ms** cross-sensor span drift (stricter than the 50 ms default) |
| Sequence completeness | `autonomy.sequence-complete` | no rig sensor dropping more than 5% of its frames |
| Ego-pose continuity | `autonomy.ego-pose-continuity` | ego trajectory continuous (no step above 100 m/s implied speed) |
| Calibration completeness | `autonomy.calibration-completeness` | connected transform (TF) tree and camera intrinsics present |

The `readiness` block on the certificate records the profile name, whether it was `applicable`, the
overall `ready` flag, and each criterion's `check_id`, `threshold`, `passed`, and finding count — and
it is signed like every other field, so a reader can trust it offline. The certificate claims nothing
beyond the criteria listed.
