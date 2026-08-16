# veridex-data

Python bindings for [Veridex](https://github.com/clay-good/veridex) — the open, cross-format
**trust layer for physical-AI data**. Point it at a robot/AV dataset and it tells you, across any
format, whether the data is clean, correctly time-synchronized, and traceable to its origin.

The distribution is `veridex-data`; the import module is `veridex`:

```sh
pip install veridex-data
```

```python
import veridex, json

report = json.loads(veridex.check("my-dataset.mcap"))
print(report["trust_score"]["grade"], report["verdict"]["status"])

# The deterministic CDM content hash of a dataset.
print(veridex.content_hash("my-dataset.mcap"))

# The Canonical Dataset Model, without running checks (same as `veridex inspect --json`).
cdm = json.loads(veridex.inspect("my-dataset.mcap"))
print(len(cdm["episodes"]), "episodes")

# The built-in check catalog (same as `veridex checks --json`).
for check in json.loads(veridex.catalog()):
    print(check["id"], "→", check["finding_codes"])

# Extract provenance as Croissant (default) or W3C PROV (same as `veridex provenance`).
croissant = json.loads(veridex.provenance("my-dataset.mcap"))
prov = json.loads(veridex.provenance("my-dataset.mcap", "prov"))

# Diff two `check` reports for CI regression gating (same as `veridex diff --json`).
old, new = veridex.check("v1.mcap"), veridex.check("v2.mcap")
delta = json.loads(veridex.diff(old, new))
print(delta["introduced"], delta["score_delta"])

# Issue and verify a signed, content-bound trust certificate (same as `veridex certify`/`verify`).
cert = veridex.certify("my-dataset.mcap", secret_key_hex)   # from `veridex keygen`
result = json.loads(veridex.verify(cert, "my-dataset.mcap"))  # raises ValueError if tampered
print(result["verified"], result["key_id"], result["trust_score"]["score"])

# Certify against a readiness profile: the certificate gains a signed per-criterion `readiness`
# block, which `verify` reports back (same as `veridex certify --profile`).
cert = veridex.certify("my-rig.mcap", secret_key_hex, profile="world-model-ready")
readiness = json.loads(veridex.verify(cert, "my-rig.mcap"))["readiness"]
print(readiness["ready"], [c["check_id"] for c in readiness["criteria"]])
```

These bindings add **no logic**: they call the exact same `veridex_core` pipeline the `veridex`
CLI calls, so verdicts, trust scores, and content hashes are byte-identical across the CLI and
Python (enforced by a parity test in CI). `veridex.check(path)` returns the same versioned JSON as
`veridex check --json`.

See the [main README](https://github.com/clay-good/veridex) for the full picture, and build the
extension locally with [maturin](https://github.com/PyO3/maturin):

```sh
maturin develop -m crates/veridex-py/Cargo.toml
```

## License

MIT.
