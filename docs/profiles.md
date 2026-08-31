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

Two kinds of profile share the flag, and they claim different things.

- A **threshold** profile — `standard`, `strict` — moves the numbers the run measures at and makes no
  other claim. There is no readiness verdict to print or sign, because it names no criteria.
- A **readiness** profile — `world-model-ready` — names criteria as well, and produces the
  per-criterion verdict a certificate signs.

## `standard`

Veridex's built-in thresholds, named. It changes nothing, which is the point: a pipeline that runs
`--profile standard` records *which* policy it ran under, so a later run under `strict` is a visible
change rather than an undocumented one.

## `strict`

The same catalog, measured harder: cross-stream drift at 20 ms instead of 50, rate deviation at 5%
instead of 10%, a gap at 2x the expected interval instead of 3x, jitter at a 0.3 coefficient of
variation instead of 0.5, an outlier at 6σ instead of 10σ, and a rig sensor allowed to drop 1% of its
frames instead of 5%.

Every one of those is *tighter* than the default, which is what makes `strict` safe to gate on.
Measuring the data harder than the catalog asks can only lower a score, so it is not a narrowing: it
emits no `SCOPE.NARROWED`, and `check --profile strict --min-score 80` is a valid CI gate. It also
cannot relax a threshold you set tighter still — among the thresholds a profile names, the stricter
of the two applies.

## There is no `lenient`

Asking for one is refused, with the reason rather than "unknown profile":

```
veridex: `lenient` is not a profile Veridex provides: a profile may only tighten a threshold, never
loosen one — a loosened run is a narrowed run, and Veridex discloses it per threshold
(`SCOPE.NARROWED`) rather than hiding it behind a name.
```

A loosened threshold does not deselect a check; the check runs, measures the defect, and passes it.
That is precisely the failure `SCOPE.NARROWED` exists to surface, per threshold and by how much — and
a run carrying it cannot be gated with `--min-score` or certified as a clean whole-catalog result.
Bundling loosened thresholds under a reassuring word would launder exactly that. Set them in your
`veridex.toml`, where the disclosure names each one.

## `world-model-ready`

For multi-sensor autonomy rigs. It tightens cross-sensor sync and bundles the autonomy criteria a
world-model training set needs. A dataset is **ready** only when the profile applies *and* every
criterion's check **ran cleanly and found nothing**.

Three rules keep that honest:

- **Applicability demands the data the criteria are about.** The profile applies to a sensor rig that
  carries a perception sensor (LiDAR or camera) *and* an ego trajectory. A bus-only measurement (a
  CAN or MF4 log) is a rig by sensor count, but calibration completeness and ego-pose continuity have
  nothing to examine there, so it is reported `N/A` rather than passing on empty criteria.
- **Silence is not a pass.** A check that was disabled in `veridex.toml`, filtered out of the run, or
  that failed internally produces no findings — so each criterion records whether its check actually
  ran, and one that didn't blocks `ready` and prints as `? … [check did not run]`. A dataset cannot
  be certified ready by switching the checks off.
- **A profile can only tighten.** Where a profile names a threshold and your `veridex.toml` sets the
  same one, the **stricter** of the two applies; thresholds the profile does not name are left
  exactly as you configured them. So `--profile world-model-ready` cannot relax a limit you set
  deliberately — asking for `clock_skew_ms = 5.0` keeps 5 ms rather than being loosened to the
  profile's 20 ms, and the readiness criterion still holds, since 5 ms is stricter than it requires.
  To see exactly which thresholds a profile moved and what it moved them from, run
  `veridex check --print-config --profile world-model-ready` — it prints every setting with the layer
  that set it, without reading a dataset.
- **A narrowed run is never ready.** The profile names only one threshold, so the rest of the
  criteria's limits come from your `veridex.toml`. Loosening one of *those* would otherwise buy a
  READY verdict whose criterion line still quoted the default: one `sequence_drop_fraction = 0.9`
  line took a rig dropping a seventh of its LiDAR frames to a signed `ready: true` beside "no rig
  sensor dropping more than 5% of its frames". So readiness is **not applicable** over any run that
  loosened a threshold, deselected a check, or overrode a severity — the same runs `--min-score`
  refuses. A profile that *tightens* is not a narrowing and is unaffected.
- **A profile run can carry `--min-score`.** Tightening measures the data harder than the catalog
  asks, so it does not narrow the run and does not block the gate:
  `check --profile world-model-ready --min-score 80` is a valid CI gate.

| Criterion | Check | Passes when |
|---|---|---|
| Rig sync | `autonomy.rig-sync` | rig sensors within a **20 ms** cross-sensor span drift (stricter than the 50 ms default) |
| Sequence completeness | `autonomy.sequence-complete` | no rig sensor dropping more than 5% of its frames |
| Ego-pose continuity | `autonomy.ego-pose-continuity` | ego trajectory continuous (no step above 100 m/s implied speed) |
| Calibration completeness | `autonomy.calibration-completeness` | a connected, unambiguous transform (TF) tree and camera intrinsics present, and arithmetically usable |
| Sensor frame resolution | `autonomy.sensor-frame-resolution` | every sensor's own frame resolves through the tree to a camera |
| GNSS plausibility | `autonomy.gnss-plausibility` | every satellite fix is a possible place, and the receiver actually had one — a drive whose fix is impossible or never acquired cannot be aligned to a map or to another drive |

The `readiness` block on the certificate records the profile name, whether it was `applicable`, the
overall `ready` flag, and each criterion's `check_id`, `threshold`, `passed`, and finding count — plus
`ran`, which is written out only when it is `false` (a criterion whose check did not run cannot pass,
and says so). It is signed like every other field, so a reader can trust it offline. The certificate claims nothing
beyond the criteria listed.

## Reading a readiness certificate back

`verify` reports what the certificate attests, not just that the signature checks out:

```sh
veridex verify my-rig.mcap --certificate my-rig.veridex.json --key issuer.pub
```

```
✓ certificate verified
  issuer key: 8f3c1d05be27a94f6021c8e3ab5d7f40912e6c8b3d0a5f7e14b9c26d8a03f5e1
  issued at:  1700000000
  dataset:    my-rig
  bound to:   4a1b9c2d7e5f0813…
  status:     pass (warnings)
  trust:      B (82)  [data 92 · provenance 66%]
  issued by:  veridex 0.1.0
  world-model-ready profile: NOT READY
    ✓ autonomy.rig-sync — rig sensors within a 20 ms cross-sensor span drift
    ✗ autonomy.sequence-complete — no rig sensor dropping more than 5% of its frames
    ✓ autonomy.ego-pose-continuity — ego trajectory continuous (no step above 100 m/s implied speed)
    ✓ autonomy.calibration-completeness — a connected, unambiguous transform (TF) tree and camera intrinsics present, and arithmetically usable
    ✓ autonomy.sensor-frame-resolution — every sensor's own frame resolves through the tree to a camera
    ✓ autonomy.gnss-plausibility — every satellite fix is a possible place, and the receiver actually had one
```

Add `--json` for the machine-readable summary (the same fields, plus the `readiness` block verbatim)
— byte-identical to what `veridex.verify(...)` returns from Python.

Nothing here is re-derived from the dataset: every line comes out of the signed document, so editing
a criterion to read `passed` makes the certificate fail verification instead of printing a nicer
verdict. Verification is fully offline.

From Python:

```python
cert = veridex.certify("my-rig.mcap", secret_key_hex, profile="world-model-ready")
# `verify` requires a trusted issuer: a valid signature only proves a certificate is
# self-consistent, and anyone can mint one. Pass the issuer's public key (from `veridex keygen`),
# or `allow_any_issuer=True` to accept any signer and get `issuer_verified: false` back.
readiness = json.loads(
    veridex.verify(cert, "my-rig.mcap", public_key_hex=issuer_pub)
)["readiness"]
```
