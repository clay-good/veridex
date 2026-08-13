"""CLI ⇄ Python parity test (design D1).

The Python bindings must produce verdicts, trust scores, and content hashes identical to the
`veridex` CLI. Run after building the extension with maturin:

    maturin develop -m crates/veridex-py/Cargo.toml
    python -m pytest crates/veridex-py/tests/test_parity.py

or, without maturin, point PYTHONPATH at a directory containing the built extension renamed to
`veridex.so` (see the repo README). VERIDEX_BIN may override the CLI binary path.
"""

import json
import os
import subprocess
import sys

import veridex  # the built extension module


def _demo_dataset(tmp_path):
    """Generate a demo MCAP via the workspace example; return its path."""
    out = tmp_path / "demo.mcap"
    subprocess.run(
        ["cargo", "run", "-q", "-p", "veridex-core", "--example", "make_demo_mcap", "--", str(out)],
        check=True,
    )
    return out


def _cli_check_json(path):
    binary = os.environ.get("VERIDEX_BIN", "target/debug/veridex")
    result = subprocess.run(
        [binary, "check", "--json", str(path)],
        capture_output=True,
        text=True,
    )
    # `check` exits non-zero on a fail verdict; that is expected, parse stdout regardless.
    return json.loads(result.stdout)


def test_cli_and_python_agree(tmp_path):
    dataset = _demo_dataset(tmp_path)

    py = json.loads(veridex.check(str(dataset)))
    cli = _cli_check_json(dataset)

    assert py == cli, "Python and CLI must produce identical reports"
    assert veridex.content_hash(str(dataset)) == py["verdict"]["cdm_content_hash"]
    assert py["trust_score"]["rubric_version"] == "v1"


def _cli_inspect_json(path):
    binary = os.environ.get("VERIDEX_BIN", "target/debug/veridex")
    result = subprocess.run(
        [binary, "inspect", "--json", str(path)],
        capture_output=True,
        text=True,
        check=True,
    )
    return json.loads(result.stdout)


def test_cli_and_python_inspect_agree(tmp_path):
    dataset = _demo_dataset(tmp_path)

    py = json.loads(veridex.inspect(str(dataset)))
    cli = _cli_inspect_json(dataset)

    assert py == cli, "Python and CLI must produce identical CDM inspection"


def _cli_catalog_json():
    binary = os.environ.get("VERIDEX_BIN", "target/debug/veridex")
    result = subprocess.run(
        [binary, "checks", "--json"],
        capture_output=True,
        text=True,
        check=True,
    )
    return json.loads(result.stdout)


def test_cli_and_python_catalog_agree():
    py = json.loads(veridex.catalog())
    cli = _cli_catalog_json()

    assert py == cli, "Python and CLI must expose the identical check catalog"
    # Sanity: the catalog is non-empty and every entry carries the documented fields.
    assert py, "catalog must not be empty"
    for check in py:
        assert {"id", "category", "default_severity", "scope", "finding_codes"} <= check.keys()


def _cli_provenance_json(path, emit):
    binary = os.environ.get("VERIDEX_BIN", "target/debug/veridex")
    result = subprocess.run(
        [binary, "provenance", str(path), "--emit", emit],
        capture_output=True,
        text=True,
        check=True,
    )
    return json.loads(result.stdout)


def test_cli_and_python_provenance_agree(tmp_path):
    dataset = _demo_dataset(tmp_path)
    for emit in ("croissant", "prov"):
        py = json.loads(veridex.provenance(str(dataset), emit))
        cli = _cli_provenance_json(dataset, emit)
        assert py == cli, f"Python and CLI provenance must agree for emit={emit}"


if __name__ == "__main__":
    # Minimal runner when pytest is unavailable.
    import tempfile
    from pathlib import Path

    with tempfile.TemporaryDirectory() as d:
        test_cli_and_python_agree(Path(d))
    print("parity OK")
    sys.exit(0)
