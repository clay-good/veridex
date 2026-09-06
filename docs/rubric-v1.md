# Veridex trust rubric — v1

The trust score is a **documented, versioned, deterministic** function of a validation verdict and
a dataset's provenance coverage. The same dataset and the same Veridex version always produce the
same score. **Scores are only comparable within the same `rubric_version`.**

`rubric_version: v1`

**What the version tracks, and what it does not.** The rubric is the *function* from a verdict to a
score: the weights below, the deductions, the two axes and how they combine. The **catalog** is an
input to that function, not part of it — so adding a check, adding a finding code, or fixing a check
that was missing a defect does **not** change the rubric, and does not bump `rubric_version`. What
it changes is the verdict, which is why the sentence above pins comparability to the same *Veridex
version* as well: a later version can find more in the same bytes, and should. Bump
`RUBRIC_VERSION` when a weight, a deduction, a grade boundary or the axis split changes — when the
same verdict would score differently.

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
`upstream`. Most are read from what a source records *about* the data; `calibration` is also
satisfied by the data itself, when a recording carries its own transform tree and camera intrinsics. Coverage is the percentage of those that are present as **known** or **asserted**
(absent or `unknown` elements do not count):

```
provenance_pct = round_down( (known + asserted) / 6 * 100 )
```

An element a **producer attested** (`veridex attest`, applied with `check --attestation`) counts as
`asserted` — the same class and the same weight as a value the source asserts rather than records.
This is the one way the provenance axis moves on a *signature* rather than on the data, so it is
disclosed in the verdict (`PROVENANCE.ATTESTED`, naming the producer key and every element it
supplied) and recorded in the certificate. A reader who does not trust that key can subtract exactly
those elements; one who cannot see them could not. An attested value never upgrades or overrides an
element the dataset already records — a contradiction is a finding, not a resolution.

An element whose value is a low-information placeholder (`unknown`, `n/a`, `none`, …) is present in
form but empty in substance, so it does **not** count as known or asserted — it is treated as
`unknown` here and flagged as `PROVENANCE.PLACEHOLDER_VALUE`. This keeps fake provenance from
inflating the score.

Coverage counts an element as present when **any** provenance record supplies it, whatever that
record's scope. Provenance is scoped in the CDM (dataset, episode, stream), so lineage recorded for
one episode of a thousand counts the same here as lineage recorded for the whole dataset — the axis
answers "does this dataset carry the element", not "for how much of it". That is a deliberate
property of rubric v1 rather than an oversight, and prorating it would move every score issued
against this rubric. What it must not do is stay invisible, so the partiality is reported instead:
`PROVENANCE.PARTIAL` names the element and how many of the run's episodes it covers, `veridex
inspect` ends the line with `(recorded for 1 of 1000 episode(s))`, and both emitted documents carry
it — the Croissant per element as `scope`, the PROV entity as `veridex:partialProvenance`. A reader
who wants a prorated number has what they need to compute one.

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

## What the score does not measure

Two limits are worth stating plainly, because both let a dataset look better than a fuller one:

- **Narrowing the run costs nothing.** A check that *errors* is charged as a coverage gap (−10), but a
  check switched off in `veridex.toml`, excluded by `categories`/`only_checks`, or downgraded through
  `severity_overrides` simply produces no findings and no deduction. Configuration is a legitimate
  tool, so the score does not second-guess it — instead the **effective config and the checks that
  ran are recorded in every verdict and signed into every certificate**, so a reader can see the
  scope a score was earned within. Compare scores only across runs with the same configuration. (For
  readiness specifically this is not left to the reader: a criterion whose check did not run cannot
  pass — see [profiles.md](profiles.md).)
- **Absent evidence beats imperfect evidence.** Several statistical checks need the source's stored
  statistics; a dataset that ships none is not judged on them, while one that ships honest-but-flawed
  stats is. Publishing less can therefore score higher. Veridex will not infer statistics it was not
  given, so the fix is to read the coverage notes alongside the score rather than to penalize
  what *is* declared.
