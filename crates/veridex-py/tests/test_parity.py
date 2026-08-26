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
    # `certify` exits with the verdict's own code (the demo carries a clock-skew error, so 20): a
    # certificate attests a verdict, including a failing one, and the exit code says which. It still
    # writes the certificate — that is the document being compared here.
    result = subprocess.run(
        [binary, "certify", str(dataset), "--key", str(keyfile), "--timestamp", ts, "--out", str(out)],
        capture_output=True,
        text=True,
    )
    assert result.returncode == 20, f"certify must report the verdict it signed: {result.stderr}"
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
    result = subprocess.run(
        [binary, "certify", str(dataset), "--key", str(keyfile), "--timestamp", ts,
         "--out", str(out), "--profile", "world-model-ready"],
        capture_output=True,
        text=True,
    )
    assert result.returncode == 20, f"certify must report the verdict it signed: {result.stderr}"
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



def _demo_lerobot(tmp_path):
    """Generate a demo LeRobot v3 dataset — the format with an episode axis to sample along."""
    out = tmp_path / "lerobot"
    subprocess.run(
        [
            "cargo", "run", "-q", "-p", "veridex-core",
            "--example", "make_demo_lerobot", "--", str(out), "clean",
        ],
        check=True,
    )
    return out


def test_cli_and_python_sampled_checks_agree(tmp_path):
    """A sampled run must be identical across the front-ends, coverage note included."""
    dataset = _demo_lerobot(tmp_path)
    binary = os.environ.get("VERIDEX_BIN", "target/debug/veridex")

    for py_kwargs, cli_flags in (
        ({"sample_episodes": 1}, ["--sample-episodes", "1"]),
        ({"sample_fraction": 0.5, "sample_seed": 7}, ["--sample-fraction", "0.5", "--sample-seed", "7"]),
    ):
        py = json.loads(veridex.check(str(dataset), **py_kwargs))
        result = subprocess.run(
            [binary, "check", "--json", str(dataset), *cli_flags],
            capture_output=True,
            text=True,
        )
        cli = json.loads(result.stdout)
        assert py == cli, f"sampled reports must agree for {cli_flags}"
        assert py["verdict"]["coverage"]["kind"] == "sample"

    # And a full check says nothing about coverage beyond being full.
    assert json.loads(veridex.check(str(dataset)))["verdict"]["coverage"]["kind"] == "full"


def test_python_rejects_the_same_sampling_mistakes_the_cli_does(tmp_path):
    dataset = _demo_lerobot(tmp_path)
    for kwargs in (
        {"sample_episodes": 1, "sample_fraction": 0.5},
        {"sample_seed": 3},
        {"sample_episodes": 0},
        {"sample_fraction": 0.0},
        {"sample_fraction": 1.5},
    ):
        try:
            veridex.check(str(dataset), **kwargs)
        except ValueError:
            continue
        raise AssertionError(f"{kwargs} must be refused, not silently ignored")


def test_cli_and_python_metadata_only_checks_agree(tmp_path):
    """A metadata-only run must be identical across the front-ends, coverage note included."""
    dataset = _demo_lerobot(tmp_path)
    binary = os.environ.get("VERIDEX_BIN", "target/debug/veridex")

    py = json.loads(veridex.check(str(dataset), metadata_only=True))
    result = subprocess.run(
        [binary, "check", "--json", str(dataset), "--metadata-only"],
        capture_output=True,
        text=True,
    )
    cli = json.loads(result.stdout)
    assert py == cli, "metadata-only reports must agree across the front-ends"
    assert py["verdict"]["coverage"]["kind"] == "metadata_only"

    # inspect too: the same manifest-derived CDM, with no frames in it.
    py_cdm = json.loads(veridex.inspect(str(dataset), metadata_only=True))
    result = subprocess.run(
        [binary, "inspect", "--json", str(dataset), "--metadata-only"],
        capture_output=True,
        text=True,
    )
    assert py_cdm == json.loads(result.stdout)
    assert py_cdm["schema"] == "veridex.inspect/1"
    # The CDM sits under `dataset`, beside what the run actually covered — the whole point of a
    # metadata-only inspect is that those two are read together.
    assert py_cdm["coverage"]["kind"] == "metadata_only"
    assert py_cdm["dataset"]["episodes"], "the declared episodes are still present"
    assert py_cdm["dataset"]["episodes"][0]["streams"][0]["frames"] == []


def test_python_refuses_a_metadata_only_sample(tmp_path):
    """The two partial coverages cannot both ride in one verdict, so asking for both is an error."""
    dataset = _demo_lerobot(tmp_path)
    try:
        veridex.check(str(dataset), metadata_only=True, sample_episodes=1)
    except ValueError:
        return
    raise AssertionError("a metadata-only sample must be refused, not silently resolved")


def test_diff_refuses_a_file_that_is_not_a_report():
    """The CLI has always refused a non-report; this binding did not, so a truncated artifact
    diffed as "every finding resolved, no regression" -- silence from a file that was never a
    report, read as a clean bill of health."""
    full = json.dumps({
        "schema": "veridex.report/1",
        "verdict": {"findings": [{"code": "X", "severity": "error"}]},
        "trust_score": {"score": 50},
    })
    truncated = json.dumps({"schema": "veridex.report/1"})

    try:
        veridex.diff(full, truncated)
    except ValueError as e:
        assert "not a Veridex report" in str(e), str(e)
    else:
        raise AssertionError("a truncated artifact must not diff as findings resolved")

    # And a real pair still diffs.
    assert json.loads(veridex.diff(full, full))["unchanged_count"] == 1


if __name__ == "__main__":
    # Minimal runner when pytest is unavailable. Defined last, so every test above it exists.
    import tempfile
    from pathlib import Path

    with tempfile.TemporaryDirectory() as d:
        test_cli_and_python_agree(Path(d))
    print("parity OK")
    sys.exit(0)


def test_cli_and_python_effective_config_agree(tmp_path):
    """The effective configuration — every value and the layer it came from — must be one document.

    The CLI reads a `veridex.toml` off disk and Python is handed its text, so the two legitimately
    disagree about the file's *name*; everything they say about what the config *means* must match.
    """
    config = tmp_path / "veridex.toml"
    config.write_text(
        "fail_on = 'warning'\n"
        "min_score = 70\n"
        "disabled_checks = ['semantic.task-quality']\n"
        "\n[tolerances]\n"
        "clock_skew_ms = 50.0\n"
        "gap_factor = 2.0\n"
    )

    binary = os.environ.get("VERIDEX_BIN", "target/debug/veridex")
    cli = json.loads(
        subprocess.run(
            [
                binary, "check", "--print-config", "--json",
                "--config", str(config),
                "--profile", "world-model-ready",
                "--min-score", "90",
            ],
            capture_output=True,
            text=True,
            check=True,
        ).stdout
    )
    py = json.loads(
        veridex.effective_config(
            config=config.read_text(), profile="world-model-ready", min_score=90
        )
    )

    assert py["settings"] == cli["settings"], "Python and CLI must resolve a config identically"
    assert py["schema"] == cli["schema"] == "veridex.config/1"
    assert py["profile"] == cli["profile"] == "world-model-ready"

    settings = {s["key"]: s for s in py["settings"]}
    # The flag beats the file, the profile beats the file, and each says so.
    assert settings["min_score"]["value"] == "90"
    assert settings["min_score"]["origin"] == "flag"
    assert settings["tolerances.clock_skew_ms"]["value"] == "20"
    assert settings["tolerances.clock_skew_ms"]["origin"] == "profile"
    assert settings["tolerances.gap_factor"]["origin"] == "config-file"
    assert settings["tolerances.jitter_cv"]["origin"] == "default"


def test_python_effective_config_reports_a_config_the_check_binding_refuses(tmp_path):
    """`veridex.check` refuses a config carrying `min_score`/`fail_on` — it cannot act on them.

    Reporting what a config *says* is a different job from running under it, and refusing here would
    make the one call that exists to explain a CI config unable to read a CI config.
    """
    text = "fail_on = 'warning'\nmin_score = 70\n"
    settings = {
        s["key"]: s for s in json.loads(veridex.effective_config(config=text))["settings"]
    }
    assert settings["fail_on"]["value"] == "warning"
    assert settings["fail_on"]["origin"] == "config-file"
    assert settings["min_score"]["value"] == "70"
    assert settings["min_score"]["origin"] == "config-file"


def test_cli_and_python_redacted_reports_agree(tmp_path):
    """A report prepared for sharing must be the same document from either front-end."""
    dataset = _demo_dataset(tmp_path)
    binary = os.environ.get("VERIDEX_BIN", "target/debug/veridex")

    py = json.loads(veridex.check(str(dataset), redact=True))
    cli = json.loads(
        subprocess.run(
            [binary, "check", "--json", "--redact", str(dataset)],
            capture_output=True,
            text=True,
        ).stdout
    )
    assert py == cli, "redacted reports must agree across the front-ends"

    codes = [f["code"] for f in py["verdict"]["findings"]]
    assert "REPORT.REDACTED" in codes, "the redaction discloses itself"
    assert "TEMPORAL.CLOCK_SKEW" in codes, "the findings survive"
    assert "/camera/image" not in json.dumps(py), "a stream name leaked"

    # The verdict is the run's own: same hash, same score, same status as the unredacted report.
    plain = json.loads(veridex.check(str(dataset)))
    assert py["verdict"]["cdm_content_hash"] == plain["verdict"]["cdm_content_hash"]
    assert py["trust_score"]["score"] == plain["trust_score"]["score"]
    assert py["verdict"]["status"] == plain["verdict"]["status"]


def test_cli_and_python_labels_agree(tmp_path):
    """The label is what gets pasted into a dataset card; both front-ends must produce one text."""
    dataset = _demo_dataset(tmp_path)
    secret = "01" * 32
    ts = "1700000000"
    cert_json = veridex.certify(str(dataset), secret, ts)
    public = veridex.keygen()[1]  # a different key, to prove the trust check bites

    binary = os.environ.get("VERIDEX_BIN", "target/debug/veridex")
    keyfile = tmp_path / "issuer"
    keyfile.write_text(secret + "\n")
    cert_path = tmp_path / "cert.json"
    cert_path.write_text(cert_json)
    cli = subprocess.run(
        [binary, "label", "--certificate", str(cert_path), "--allow-any-issuer"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    py = veridex.label(cert_json, allow_any_issuer=True)
    assert py == cli, "Python and CLI must render one label"
    assert "Veridex trust label" in py
    assert "statement of fact" in py
    assert "Issuer not verified" in py, "the caveat travels with the text"

    # A trust decision is required, and a wrong key is refused rather than labeled.
    try:
        veridex.label(cert_json)
    except ValueError:
        pass
    else:
        raise AssertionError("label must demand a trust decision about the issuer")
    try:
        veridex.label(cert_json, public_key_hex=public)
    except ValueError:
        pass
    else:
        raise AssertionError("a certificate from another issuer must not be labeled")


def test_cli_and_python_attestations_agree(tmp_path):
    """A signed attestation must be one document, and raise provenance identically."""
    dataset = _demo_lerobot(tmp_path)
    secret = "01" * 32
    ts = "1700000000"

    py_doc = veridex.attest(
        str(dataset), secret, {"clock": "ptp-grandmaster", "annotator": "dana"}, ts
    )

    binary = os.environ.get("VERIDEX_BIN", "target/debug/veridex")
    keyfile = tmp_path / "producer"
    keyfile.write_text(secret + "\n")
    out = tmp_path / "a.json"
    subprocess.run(
        [
            binary, "attest", str(dataset),
            "--key", str(keyfile),
            "--set", "clock=ptp-grandmaster",
            "--set", "annotator=dana",
            "--out", str(out),
            "--timestamp", ts,
        ],
        capture_output=True,
        text=True,
        check=True,
    )
    assert json.loads(py_doc) == json.loads(out.read_text()), "one attestation, both front-ends"

    # Applying it raises provenance coverage and leaves the dataset's identity alone.
    plain = json.loads(veridex.check(str(dataset)))
    attested = json.loads(veridex.check(str(dataset), attestation=py_doc))
    assert attested["trust_score"]["provenance_pct"] > plain["trust_score"]["provenance_pct"]
    assert attested["verdict"]["cdm_content_hash"] == plain["verdict"]["cdm_content_hash"]
    codes = [f["code"] for f in attested["verdict"]["findings"]]
    assert "PROVENANCE.ATTESTED" in codes, "the run says a signature is why"

    # The same document applied to different data is refused rather than counted.
    other = _demo_dataset(tmp_path)
    try:
        veridex.check(str(other), attestation=py_doc)
    except ValueError as e:
        assert "different dataset" in str(e), e
    else:
        raise AssertionError("an attestation about other data must not apply")


def test_python_attest_refuses_a_key_veridex_does_not_score(tmp_path):
    dataset = _demo_lerobot(tmp_path)
    try:
        veridex.attest(str(dataset), "01" * 32, {"favourite_colour": "blue"})
    except ValueError as e:
        assert "not a provenance element" in str(e), e
    else:
        raise AssertionError("an unscored key must be refused, not signed")


def test_cli_and_python_attested_certificates_agree(tmp_path):
    """A certificate issued with an attestation must be one document from either front-end — and
    must not contradict itself: the coverage block and the trust score describe the same run."""
    dataset = _demo_lerobot(tmp_path)
    producer_secret = "02" * 32
    issuer_secret = "01" * 32
    ts = "1700000000"

    attestation = veridex.attest(str(dataset), producer_secret, {"clock": "ptp"}, ts)
    py_cert = json.loads(
        veridex.certify(str(dataset), issuer_secret, ts, attestation=attestation)
    )

    binary = os.environ.get("VERIDEX_BIN", "target/debug/veridex")
    (tmp_path / "issuer").write_text(issuer_secret + "\n")
    (tmp_path / "a.json").write_text(attestation)
    out = tmp_path / "cert.json"
    subprocess.run(
        [
            binary, "certify", str(dataset),
            "--key", str(tmp_path / "issuer"),
            "--timestamp", ts,
            "--attestation", str(tmp_path / "a.json"),
            "--out", str(out),
        ],
        capture_output=True,
        text=True,
    )
    assert py_cert == json.loads(out.read_text()), "one certificate, both front-ends"

    cert = py_cert["certificate"]
    coverage = cert["provenance_coverage"]
    assert coverage["asserted"] == 1, coverage
    assert (coverage["known"] + coverage["asserted"]) * 100 // 6 == cert["trust_score"][
        "provenance_pct"
    ], "the coverage block and the trust score must describe the same run"
    assert cert["attestation"]["keys"] == ["clock"]


def test_cli_and_python_attested_emits_agree(tmp_path):
    """An emit carrying attested provenance must be one document from either front-end."""
    dataset = _demo_lerobot(tmp_path)
    attestation = veridex.attest(str(dataset), "02" * 32, {"clock": "ptp"}, "1700000000")
    (tmp_path / "a.json").write_text(attestation)
    binary = os.environ.get("VERIDEX_BIN", "target/debug/veridex")

    for emit in ("croissant", "prov"):
        cli = json.loads(
            subprocess.run(
                [binary, "provenance", str(dataset), "--emit", emit,
                 "--attestation", str(tmp_path / "a.json")],
                capture_output=True, text=True, check=True,
            ).stdout
        )
        py = json.loads(veridex.provenance(str(dataset), emit, attestation=attestation))
        assert py == cli, f"attested {emit} must agree across the front-ends"

    croissant = json.loads(veridex.provenance(str(dataset), "croissant", attestation=attestation))
    assert croissant["veridex:attestedBy"]["keys"] == ["clock"]
    classes = {e["key"]: e["class"] for e in croissant["veridex:provenance"]}
    assert classes["clock"] == "asserted", classes
    # The bound hash is the dataset's own, with or without the attestation.
    plain = json.loads(veridex.provenance(str(dataset), "croissant"))
    assert croissant["distribution"][0]["sha256"] == plain["distribution"][0]["sha256"]
