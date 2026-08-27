# veridex-cli

The `veridex` command line — [Veridex](https://github.com/clay-good/veridex)'s primary surface.

```sh
cargo install veridex-cli
veridex check my-dataset/
```

One command over LeRobot v3, RLDS/TFDS, HDF5, Zarr, MCAP, ROS 2 rosbag2, CAN+DBC and ASAM MDF/MF4: it reports
whether the data is structurally sound, correctly time-synchronized, and traceable to its origin,
scores it 0–100, and can stamp it with a signed certificate that verifies offline.

| Command | What it does |
| --- | --- |
| `veridex check <dataset>` | validate and report (`--json`, `--sarif`, `--html`, `--redact`) |
| `veridex watch <dataset>` | re-validate as the dataset is recorded |
| `veridex certify <dataset> --key issuer.key` | issue a signed trust certificate |
| `veridex verify <dataset> --certificate c.json --key pub.key` | verify one, offline |
| `veridex provenance <dataset> --emit croissant` | emit Croissant / W3C PROV |
| `veridex inspect <dataset>` · `veridex diff a.json b.json` · `veridex checks` | summarize, compare, list the catalog |

Exit codes are the CI contract: `0` pass, `10` pass-with-warnings, `20` fail, `2` tool error.
Veridex only ever reads your data.

Quickstart, configuration, and the full check catalog:
[github.com/clay-good/veridex](https://github.com/clay-good/veridex). MIT licensed.
