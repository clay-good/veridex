# Veridex trust rubric — v1

The trust score is a **documented, versioned, deterministic** function of a validation verdict and
a dataset's provenance coverage. The same dataset and the same Veridex version always produce the
same score. **Scores are only comparable within the same `rubric_version`.**

`rubric_version: v1`

## Two axes

The score combines two independent axes so a clean data verdict can never mask missing provenance.

### 1. Data quality (70% weight)

Start at 100 and deduct for each **non-provenance** finding and each check that failed to run:

| Signal | Deduction |
|---|---|
| `error` finding | −15 |
| `warning` finding | −4 |
| `info` finding | 0 |
| check that errored (failed to run) | −10 |

Floor the result at 0. Provenance-category findings are excluded here — they are scored on axis 2.

### 2. Provenance coverage (30% weight)

Veridex expects six provenance elements: `license`, `sensor`, `clock`, `calibration`, `annotator`,
`upstream`. Coverage is the percentage of those that are present as **known** or **asserted**
(absent or `unknown` elements do not count):

```
provenance_pct = round_down( (known + asserted) / 6 * 100 )
```

An element whose value is a low-information placeholder (`unknown`, `n/a`, `none`, …) is present in
form but empty in substance, so it does **not** count as known or asserted — it is treated as
`unknown` here and flagged as `PROVENANCE.PLACEHOLDER_VALUE`. This keeps fake provenance from
inflating the score.

## Overall score and grade

```
score = floor( (data_score * 7 + provenance_pct * 3) / 10 )
```

| Score | Grade |
|---|---|
| 90–100 | A |
| 80–89 | B |
| 70–79 | C |
| 60–69 | D |
| < 60 | F |

Because provenance carries 30% weight, a dataset with **zero provenance** cannot score above 70
(a C) no matter how clean its data. The certificate always shows both sub-scores.

## Design notes

- Integer arithmetic throughout — no floating-point rounding to make reproducibility fragile.
- Deliberately simple (design D7): correctness and determinism over cleverness. Weights can be
  refined in a future rubric version without invalidating v1 certificates, which stay pinned to
  `rubric_version: v1`.
