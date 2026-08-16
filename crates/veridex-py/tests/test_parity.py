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


def test_cli_and_python_diff_agree(tmp_path):
    old = str(tmp_path / "old.json")
    new = str(tmp_path / "new.json")
    old_json = '{"verdict":{"findings":[{"code":"A","severity":"error","message":"m"}]},"trust_score":{"score":80}}'
    new_json = '{"verdict":{"findings":[{"code":"A","severity":"error","message":"m"},{"code":"B","severity":"warning","message":"m"}]},"trust_score":{"score":70}}'
    with open(old, "w") as f:
        f.write(old_json)
    with open(new, "w") as f:
        f.write(new_json)

    binary = os.environ.get("VERIDEX_BIN", "target/debug/veridex")
    cli = json.loads(
        subprocess.run(
            [binary, "diff", "--json", old, new],
            capture_output=True,
            text=True,
            check=True,
        ).stdout
    )
    py = json.loads(veridex.diff(old_json, new_json))
    assert py == cli, "Python and CLI diff must agree"


def test_cli_and_python_certify_and_verify_agree(tmp_path):
    dataset = _demo_dataset(tmp_path)
    secret = "01" * 32  # a 32-byte Ed25519 seed as hex
    ts = "1700000000"  # fixed timestamp so both sides sign the identical certificate

    # Ed25519 signing is deterministic: same key + same certificate bytes → identical signature.
    py_cert = veridex.certify(str(dataset), secret, ts)

    binary = os.environ.get("VERIDEX_BIN", "target/debug/veridex")
    keyfile = tmp_path / "issuer"
    keyfile.write_text(secret + "\n")
    out = tmp_path / "cert.json"
    subprocess.run(
        [binary, "certify", str(dataset), "--key", str(keyfile), "--timestamp", ts, "--out", str(out)],
        check=True,
    )
    assert json.loads(py_cert) == json.loads(out.read_text()), "Python and CLI must issue the identical certificate"

    # Python verify accepts the certificate against the same dataset and a trusted issuer key.
    public = veridex.keygen()[1]  # a different key, to prove the trust check bites
    result = json.loads(veridex.verify(py_cert, str(dataset), allow_any_issuer=True))
    assert result["verified"] is True
    assert result["issuer_verified"] is False
    assert public  # (the untrusted-key rejection is asserted below)

    # Without a trusted issuer and without opting out, verify refuses rather than implying trust.
    try:
        veridex.verify(py_cert, str(dataset))
    except ValueError:
        pass
    else:
        raise AssertionError("verify must demand a trust decision about the issuer")

    # A certificate signed by a different issuer key is rejected.
    try:
        veridex.verify(py_cert, str(dataset), "00" * 32)
    except ValueError:
        pass
    else:
        raise AssertionError("verify must reject an untrusted issuer key")


def test_cli_and_python_readiness_certificates_agree(tmp_path):
    """A profiled certificate must be byte-identical across surfaces, and verify the same way."""
    dataset = _demo_dataset(tmp_path)
    secret = "01" * 32
    ts = "1700000000"

    py_cert = veridex.certify(str(dataset), secret, ts, None, "world-model-ready")

    binary = os.environ.get("VERIDEX_BIN", "target/debug/veridex")
    keyfile = tmp_path / "issuer"
    keyfile.write_text(secret + "\n")
    out = tmp_path / "cert.json"
    subprocess.run(
        [binary, "certify", str(dataset), "--key", str(keyfile), "--timestamp", ts,
         "--out", str(out), "--profile", "world-model-ready"],
        check=True,
    )
    assert json.loads(py_cert) == json.loads(out.read_text()), "profiled certificates must match"

    # Verification summaries must match too, readiness block included.
    py_verified = json.loads(veridex.verify(py_cert, str(dataset), allow_any_issuer=True))
    cli_verified = json.loads(
        subprocess.run(
            [binary, "verify", str(dataset), "--certificate", str(out), "--allow-any-issuer", "--json"],
            capture_output=True,
            text=True,
            check=True,
        ).stdout
    )
    assert py_verified == cli_verified, "Python and CLI verification summaries must agree"
    assert py_verified["readiness"]["profile"] == "world-model-ready"

    # An unknown profile is an error on both sides, never a silently unprofiled certificate.
    try:
        veridex.certify(str(dataset), secret, ts, None, "nope")
    except ValueError:
        pass
    else:
        raise AssertionError("certify must reject an unknown profile")


def test_python_keygen_certify_verify_roundtrip(tmp_path):
    dataset = _demo_dataset(tmp_path)
    secret, public = veridex.keygen()
    assert len(secret) == 64 and len(public) == 64

    cert = veridex.certify(str(dataset), secret)
    # The certificate verifies against the dataset and the generated public key.
    result = json.loads(veridex.verify(cert, str(dataset), public))
    assert result["verified"] is True
    assert result["issuer_verified"] is True
    assert result["key_id"] == public


if __name__ == "__main__":
    # Minimal runner when pytest is unavailable.
    import tempfile
    from pathlib import Path

    with tempfile.TemporaryDirectory() as d:
        test_cli_and_python_agree(Path(d))
    print("parity OK")
    sys.exit(0)
