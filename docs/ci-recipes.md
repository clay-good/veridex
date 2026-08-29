# Running Veridex in CI

Copy-pasteable recipes. Every command here was run against this repo's demo dataset before it was
written down.

## The contract CI depends on

| Exit code | Meaning |
| --- | --- |
| `0` | pass — no findings above `info` |
| `10` | pass with warnings |
| `20` | fail — at least one error, or a gate (`--min-score`, `--fail-on warning`) was not met |
| `2` | tool error — bad usage, unreadable dataset, a config that names a check that does not exist |

`2` is not a verdict about your data. A job that treats it as a failure is right to; a job that
treats it as "the data is bad" is not.

## GitHub Actions

### Gate a dataset on every PR

```yaml
name: dataset
on: [pull_request]

jobs:
  veridex:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo install veridex-cli
      - name: Check the dataset
        run: veridex check datasets/pick-place --min-score 80
```

`--min-score` is refused — with exit `2`, loudly — over any run that cannot support the claim: a
`--metadata-only` run, a sampled run, or one narrowed by config. That is deliberate: each of those
*raises* the score by measuring less, so a gate that quietly honored them would pass precisely the
runs it exists to catch.

### Surface findings in the Security tab

```yaml
      - name: Check the dataset
        run: veridex check datasets/pick-place --sarif --out veridex.sarif
        continue-on-error: true          # upload the findings even when the gate fails
      - uses: github/codeql-action/upload-sarif@v3
        with:
          sarif_file: veridex.sarif
```

Findings arrive as code-scanning alerts with their rule, severity, risk and remedy. They carry
*logical* locations (dataset / episode / stream), not file positions — a dataset is not source code,
and Veridex will not invent a line number to annotate.

### Fail only on a regression, not on pre-existing findings

Adopting Veridex on a dataset that already has findings is easier if the gate is about *change*.
Store a baseline report and compare:

```yaml
      - run: veridex check datasets/pick-place --json --out new.json
        continue-on-error: true
      - run: veridex diff --fail-on-regression baseline.json new.json
```

`diff` fails the job when the new report introduces a finding, drops the trust score, crashes a check
that used to run, or — the four that matter most — when the two reports are about **different
datasets**, **cover different amounts** of the dataset, **one of them is redacted**, or they were
produced by **different Veridex versions**. Each of those makes the comparison meaningless, so it is
treated as a regression rather than an improvement.

The version one is the one to expect: a release that adds a check produces findings under
`introduced` on a dataset that did not change by a byte, so the first gate run after an upgrade
fails. It fails *by name* — `these reports were produced by different Veridex versions (0.1.0 ->
0.2.0)` — rather than quoting a finding count that would send you to audit sound data. Re-baseline by
storing a fresh `veridex check --json` from the new version, then read what changed once, on purpose. The dataset check is on the dataset's *id*, not its content: a dataset that
gained an episode since the baseline hashes differently, and comparing those two reports is the whole
point — a baseline artifact from another project is the mistake being caught.

## GitLab CI

```yaml
veridex:
  image: rust:latest
  script:
    - cargo install veridex-cli
    - veridex check datasets/pick-place --json --out report.json
  artifacts:
    when: always
    paths: [report.json]
```

## Pinning the policy your CI runs under

Put the thresholds in the repo, so a change to them shows up in review rather than in a job log:

```toml
# veridex.toml
fail_on = "error"
min_score = 80

[tolerances]
clock_skew_ms = 20      # this rig's cameras and arm are wired to one clock
```

Or set them from the environment, for a container that has no place to put a file:

```sh
VERIDEX_MIN_SCORE=80 VERIDEX_TOLERANCE_CLOCK_SKEW_MS=20 veridex check datasets/pick-place
```

Precedence is built-in defaults, then the file, then the environment, then flags. To see what a run
would actually use — and which layer set each value — run:

```sh
veridex check --print-config --profile strict
```

## Sharing the result

- `veridex check --redact` replaces dataset, stream, task and provenance identifiers with stable
  placeholders, for a report that can go to a customer or a public issue. The findings and every
  measurement stay.
- `veridex certify … --key issuer.key` signs the verdict, and `veridex label --certificate c.json
  --key issuer.pub` renders it as Markdown for a dataset card. A certificate verifies offline;
  nothing in the trust path is a service.

## What not to do

- **Do not gate on a sampled or metadata-only run.** Veridex refuses it, but the reason is worth
  knowing: the episodes a sample skips are exactly where the defect it is meant to catch would be.
- **Do not compare reports across Veridex versions.** A new check changes scores by design. The
  version is in every report and every certificate; compare within one.
- **Do not treat exit `2` as a data verdict.** It means Veridex could not do the job it was asked to.
