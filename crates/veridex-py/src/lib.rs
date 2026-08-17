//! Python bindings for Veridex — `pip install veridex-data`, then `import veridex`.
//!
//! These bindings add **no logic**: they call the exact same `veridex_core` pipeline the CLI calls
//! ([`veridex_core::run_check`]), so verdicts, trust scores, content hashes, and certificates are
//! identical across the CLI and Python (design D1). The `check` function returns the same versioned
//! JSON as `veridex check --json`.

// The `#[pyfunction]` macro expands to an identity `PyErr -> PyErr` conversion that clippy flags;
// it is macro-generated, not our code.
#![allow(clippy::useless_conversion)]

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use veridex_core::adapter::{IngestOptions, Sample, Source};

/// Map a core ingest error to a Python `ValueError`.
fn to_py_err(e: impl std::fmt::Display) -> PyErr {
    PyValueError::new_err(e.to_string())
}

fn source_for(path: &str) -> Source {
    Source::Local(std::path::PathBuf::from(path))
}

/// Current Unix time (seconds) as a string. The core never reads a clock (certificate timestamps are
/// caller-supplied); this binding fills one in when the caller doesn't, matching the CLI's default.
fn unix_timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

/// Build the ingest options for a sampling request, mirroring the CLI's `--sample-*` flags.
///
/// `sample_episodes` and `sample_fraction` are mutually exclusive, and a seed without a fraction
/// selects nothing — both are errors here for the same reason they are on the CLI: an argument that
/// is silently dropped leaves the caller believing it took effect.
fn ingest_options_from(
    sample_episodes: Option<u64>,
    sample_fraction: Option<f64>,
    sample_seed: u64,
    metadata_only: bool,
) -> PyResult<IngestOptions> {
    let value_error = |m: String| pyo3::exceptions::PyValueError::new_err(m);
    let sample = match (sample_episodes, sample_fraction) {
        (Some(_), Some(_)) => {
            return Err(value_error(
                "sample_episodes and sample_fraction cannot both be given".into(),
            ))
        }
        (Some(n), None) => {
            // Mirrors the CLI: a seed only makes sense for the random draw, so accepting one here and
            // discarding it would leave the caller believing it took effect.
            if sample_seed != 0 {
                return Err(value_error(
                    "sample_seed applies to sample_fraction; sample_episodes is not a random draw"
                        .into(),
                ));
            }
            Sample::FirstEpisodes(n)
        }
        (None, Some(fraction)) => Sample::Fraction {
            fraction,
            seed: sample_seed,
        },
        (None, None) => {
            if sample_seed != 0 {
                return Err(value_error("sample_seed requires sample_fraction".into()));
            }
            Sample::All
        }
    };
    sample.validate().map_err(|e| value_error(e.to_string()))?;
    Ok(IngestOptions {
        sample,
        metadata_only,
        ..IngestOptions::default()
    })
}

/// `veridex.check(path, format=None, config=None, sample_episodes=None, sample_fraction=None,
/// sample_seed=0, metadata_only=False) -> str`
///
/// Validate a dataset and return the report as a JSON string, byte-identical to
/// `veridex check --json`.
///
/// `config` is the *contents* of a `veridex.toml` (not a path). Unlike the CLI, Python never
/// auto-discovers a config file — an import should not pick up behavior from the working directory —
/// so pass it explicitly to get the same verdict the CLI gives under that config. Check ids are
/// validated, so a typo in `disabled_checks` raises rather than silently no-opping.
///
/// The sampling arguments mirror `--sample-episodes` / `--sample-fraction` / `--sample-seed`. A
/// sampled report carries `"coverage": {"kind": "sample", …}`; it is a verdict over those episodes
/// alone, and cannot be certified.
///
/// `metadata_only=True` mirrors `--metadata-only`: the manifest, the stored statistics, and the
/// provenance are checked without reading any stream payload. The report carries
/// `"coverage": {"kind": "metadata_only", …}`, and it cannot be certified either.
#[pyfunction]
#[pyo3(signature = (path, format=None, config=None, sample_episodes=None, sample_fraction=None, sample_seed=0, metadata_only=false))]
fn check(
    path: &str,
    format: Option<&str>,
    config: Option<&str>,
    sample_episodes: Option<u64>,
    sample_fraction: Option<f64>,
    sample_seed: u64,
    metadata_only: bool,
) -> PyResult<String> {
    let registry = veridex_core::default_registry();
    let run_config = run_config_from(config)?;
    let opts = ingest_options_from(sample_episodes, sample_fraction, sample_seed, metadata_only)?;
    let out =
        veridex_core::run_check_with(&registry, &source_for(path), format, &opts, &run_config)
            .map_err(to_py_err)?;
    Ok(veridex_core::render_json(&out.verdict, Some(out.trust)))
}

/// Parse optional `veridex.toml` contents into a [`RunConfig`], validating every check id it names.
fn run_config_from(config: Option<&str>) -> PyResult<veridex_core::RunConfig> {
    let Some(text) = config else {
        return Ok(veridex_core::RunConfig::default());
    };
    let parsed = veridex_core::CheckConfig::from_toml(text)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
    let engine = veridex_core::checks::default_engine()
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
    parsed
        .validate_check_ids(engine.check_ids())
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
    Ok(parsed.to_run_config())
}

/// `veridex.content_hash(path, format=None) -> str`
///
/// The deterministic CDM content hash (hex) of a dataset.
#[pyfunction]
#[pyo3(signature = (path, format=None))]
fn content_hash(path: &str, format: Option<&str>) -> PyResult<String> {
    let registry = veridex_core::default_registry();
    let out = veridex_core::run_check(
        &registry,
        &source_for(path),
        format,
        &IngestOptions::default(),
    )
    .map_err(to_py_err)?;
    Ok(veridex_core::content_hash(&out.ingested.dataset).to_hex())
}

/// `veridex.inspect(path, format=None, sample_episodes=None, sample_fraction=None, sample_seed=0,
/// metadata_only=False) -> str`
///
/// Ingest a dataset and return its Canonical Dataset Model as a JSON string, identical to
/// `veridex inspect --json`. Runs no checks. The sampling arguments behave as in
/// [`check`](fn.check.html).
#[pyfunction]
#[pyo3(signature = (path, format=None, sample_episodes=None, sample_fraction=None, sample_seed=0, metadata_only=false))]
fn inspect(
    path: &str,
    format: Option<&str>,
    sample_episodes: Option<u64>,
    sample_fraction: Option<f64>,
    sample_seed: u64,
    metadata_only: bool,
) -> PyResult<String> {
    let registry = veridex_core::default_registry();
    let source = source_for(path);
    let opts = ingest_options_from(sample_episodes, sample_fraction, sample_seed, metadata_only)?;
    let ingested = match format {
        Some(fmt) => registry.ingest_as(fmt, &source, &opts),
        None => registry.ingest(&source, &opts),
    }
    .map_err(to_py_err)?;
    let mut dataset = ingested.dataset;
    dataset.canonicalize_order();
    serde_json::to_string_pretty(&dataset).map_err(to_py_err)
}

/// `veridex.provenance(path, emit="croissant", format=None) -> str`
///
/// Extract the dataset's provenance and emit it as a JSON string — `croissant` (MLCommons Croissant
/// JSON-LD, the default) or `prov` (minimal W3C PROV) — byte-identical to `veridex provenance`.
#[pyfunction]
#[pyo3(signature = (path, emit="croissant", format=None))]
fn provenance(path: &str, emit: &str, format: Option<&str>) -> PyResult<String> {
    let registry = veridex_core::default_registry();
    let source = source_for(path);
    let opts = IngestOptions::default();
    let ingested = match format {
        Some(fmt) => registry.ingest_as(fmt, &source, &opts),
        None => registry.ingest(&source, &opts),
    }
    .map_err(to_py_err)?;
    let mut dataset = ingested.dataset;
    dataset.canonicalize_order();
    veridex_core::render_provenance(&dataset, emit).map_err(to_py_err)
}

/// `veridex.catalog() -> str`
///
/// The built-in check catalog as a JSON string, byte-identical to `veridex checks --json`: each
/// check's id, title, category, default severity, scope, version, and the finding codes it can emit.
#[pyfunction]
fn catalog() -> PyResult<String> {
    let engine = veridex_core::checks::default_engine().map_err(to_py_err)?;
    Ok(veridex_core::render_catalog_json(&engine.catalog()))
}

/// `veridex.keygen() -> (str, str)`
///
/// Generate a fresh Ed25519 signing keypair, returning `(secret_key_hex, public_key_hex)`. The
/// secret key feeds `certify`; the public key feeds `verify` (and identifies the issuer). Keep the
/// secret key private — anyone holding it can issue certificates in your name.
#[pyfunction]
fn keygen() -> (String, String) {
    let kp = veridex_core::SigningKeypair::generate();
    (kp.secret_hex(), kp.public_hex())
}

/// `veridex.certify(path, secret_key_hex, timestamp=None, format=None, profile=None) -> str`
///
/// Validate a dataset and issue a signed, content-bound trust certificate as a JSON string,
/// byte-identical to `veridex certify` for the same key and timestamp (Ed25519 signing is
/// deterministic). `secret_key_hex` is a 64-character hex secret key (see `veridex keygen`);
/// `timestamp` defaults to the current Unix time if omitted. `profile` names a readiness profile
/// (e.g. `world-model-ready`), which applies the profile's tolerances and attaches the signed
/// per-criterion `readiness` block — exactly as `veridex certify --profile` does.
#[pyfunction]
#[pyo3(signature = (path, secret_key_hex, timestamp=None, format=None, profile=None))]
fn certify(
    path: &str,
    secret_key_hex: &str,
    timestamp: Option<&str>,
    format: Option<&str>,
    profile: Option<&str>,
) -> PyResult<String> {
    let keypair = veridex_core::SigningKeypair::from_secret_hex(secret_key_hex)
        .ok_or_else(|| PyValueError::new_err("secret_key_hex is not a valid 32-byte hex key"))?;
    // An unknown profile name is an error, never a silently ignored argument that would produce a
    // certificate claiming less than the caller asked for.
    let profile = match profile {
        None => None,
        Some(name) => Some(veridex_core::profile::by_name(name).ok_or_else(|| {
            PyValueError::new_err(format!(
                "unknown profile `{name}` (known: world-model-ready)"
            ))
        })?),
    };
    let registry = veridex_core::default_registry();
    // A profile applies its own tolerances to the run (e.g. tighter cross-sensor sync).
    let run_config = veridex_core::RunConfig {
        tolerances: profile.as_ref().map(|p| p.tolerances).unwrap_or_default(),
        ..veridex_core::RunConfig::default()
    };
    let out = veridex_core::run_check_with(
        &registry,
        &source_for(path),
        format,
        &IngestOptions::default(),
        &run_config,
    )
    .map_err(to_py_err)?;
    // The same refusal the CLI makes, made here too. `Certificate::certifiable` documents that both
    // front-ends call it; only the CLI did. Satisfied today because this function exposes no
    // sampling or metadata-only argument, so the ingest is always full — but the guard being absent
    // rather than satisfied is one added parameter away from issuing a certificate over a run that
    // never read the data it attests.
    veridex_core::Certificate::certifiable(&out.verdict).map_err(PyValueError::new_err)?;
    let mut cert = veridex_core::Certificate::build(
        out.ingested.dataset.id.clone(),
        &out.verdict,
        out.trust.clone(),
        veridex_core::ProvenanceCoverage::of(&out.ingested.dataset),
        veridex_core::Issuance {
            key_id: keypair.public_hex(),
            timestamp: timestamp.map(String::from).unwrap_or_else(unix_timestamp),
        },
    );
    if let Some(p) = &profile {
        cert.readiness = Some(veridex_core::certificate::ReadinessReport::evaluate(
            p,
            &out.verdict,
            &out.ingested.dataset,
        ));
    }
    let signed = veridex_core::sign(cert, &keypair);
    serde_json::to_string_pretty(&signed).map_err(to_py_err)
}

/// `veridex.verify(certificate_json, path=None, public_key_hex=None, format=None) -> str`
///
/// Verify a certificate offline. When `path` is given the certificate must be bound to that dataset
/// (content-hash / transplant check); when `public_key_hex` is given it must be signed by that
/// issuer — which is required unless `allow_any_issuer=True`, because a signature alone only proves
/// the document is self-consistent. Returns a JSON summary on success — identical to
/// `veridex verify --json`, carrying the
/// trust score and the signed `readiness` block when the certificate has one; raises `ValueError`
/// if verification fails.
#[pyfunction]
#[pyo3(signature = (certificate_json, path=None, public_key_hex=None, format=None, allow_any_issuer=false))]
fn verify(
    certificate_json: &str,
    path: Option<&str>,
    public_key_hex: Option<&str>,
    format: Option<&str>,
    allow_any_issuer: bool,
) -> PyResult<String> {
    // A signature only proves the certificate came from *some* key. Requiring a trusted issuer (or an
    // explicit opt-out) keeps a self-issued certificate from reading as an endorsement.
    if public_key_hex.is_none() && !allow_any_issuer {
        return Err(PyValueError::new_err(
            "verify needs a trusted issuer: pass public_key_hex, or allow_any_issuer=True to check \
             only that the certificate is internally consistent",
        ));
    }
    let signed: veridex_core::SignedCertificate =
        serde_json::from_str(certificate_json).map_err(to_py_err)?;
    let presented = match path {
        Some(p) => {
            let registry = veridex_core::default_registry();
            let source = source_for(p);
            let opts = IngestOptions::default();
            let ingested = match format {
                Some(fmt) => registry.ingest_as(fmt, &source, &opts),
                None => registry.ingest(&source, &opts),
            }
            .map_err(to_py_err)?;
            Some(veridex_core::content_hash(&ingested.dataset).to_hex())
        }
        None => None,
    };
    let v = veridex_core::verify(&signed, presented.as_deref(), public_key_hex)
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(veridex_core::verified_json(
        &signed,
        &v,
        public_key_hex.is_some(),
    ))
}

/// `veridex.diff(old_report_json, new_report_json) -> str`
///
/// Diff two `veridex check --json` reports and return the machine-readable summary
/// (`{coverage, introduced, resolved, unchanged_count, score_delta}`) as a JSON string,
/// byte-identical to `veridex diff --json`. Inputs are the report JSON strings, not file paths.
///
/// Raises `ValueError` for anything that is not a Veridex report. The CLI has always refused those;
/// this binding did not, so a truncated or wrong-shaped artifact -- a `{"schema": ...}` stub from an
/// interrupted write, say -- diffed as "every finding resolved, no regression". Silence from a file
/// that was never a report is the worst possible clean bill of health.
#[pyfunction]
fn diff(old_report_json: &str, new_report_json: &str) -> PyResult<String> {
    let old: serde_json::Value = serde_json::from_str(old_report_json).map_err(to_py_err)?;
    let new: serde_json::Value = serde_json::from_str(new_report_json).map_err(to_py_err)?;
    for (label, value) in [("old", &old), ("new", &new)] {
        if !veridex_core::is_report_shaped(value) {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "the {label} report is not a Veridex report: expected the JSON written by \
                 `veridex check --json` (a `verdict.findings` array). Diffing it would read every \
                 finding as resolved."
            )));
        }
    }
    Ok(veridex_core::render_diff_json(&old, &new))
}

/// `veridex.check_sarif(path, format=None, config=None) -> str`
///
/// Validate a dataset and return the report as SARIF 2.1.0 JSON, byte-identical to
/// `veridex check --sarif`. For CI code-scanning pipelines driven from Python.
#[pyfunction]
#[pyo3(signature = (path, format=None, config=None))]
fn check_sarif(path: &str, format: Option<&str>, config: Option<&str>) -> PyResult<String> {
    let registry = veridex_core::default_registry();
    let run_config = run_config_from(config)?;
    let out = veridex_core::run_check_with(
        &registry,
        &source_for(path),
        format,
        &IngestOptions::default(),
        &run_config,
    )
    .map_err(to_py_err)?;
    Ok(
        serde_json::to_string_pretty(&veridex_core::render_sarif(&out.verdict))
            .expect("sarif serializes"),
    )
}

/// `veridex.check_html(path, format=None, config=None) -> str`
///
/// Validate a dataset and return the self-contained HTML report, byte-identical to
/// `veridex check --html`.
#[pyfunction]
#[pyo3(signature = (path, format=None, config=None))]
fn check_html(path: &str, format: Option<&str>, config: Option<&str>) -> PyResult<String> {
    let registry = veridex_core::default_registry();
    let run_config = run_config_from(config)?;
    let out = veridex_core::run_check_with(
        &registry,
        &source_for(path),
        format,
        &IngestOptions::default(),
        &run_config,
    )
    .map_err(to_py_err)?;
    Ok(veridex_core::render_html(&out.verdict, Some(out.trust)))
}

/// `veridex.version() -> str`
#[pyfunction]
fn version() -> &'static str {
    veridex_core::VERSION
}

/// The `veridex` Python module.
#[pymodule]
fn veridex(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", veridex_core::VERSION)?;
    m.add_function(wrap_pyfunction!(check, m)?)?;
    m.add_function(wrap_pyfunction!(check_sarif, m)?)?;
    m.add_function(wrap_pyfunction!(check_html, m)?)?;
    m.add_function(wrap_pyfunction!(content_hash, m)?)?;
    m.add_function(wrap_pyfunction!(inspect, m)?)?;
    m.add_function(wrap_pyfunction!(catalog, m)?)?;
    m.add_function(wrap_pyfunction!(provenance, m)?)?;
    m.add_function(wrap_pyfunction!(diff, m)?)?;
    m.add_function(wrap_pyfunction!(keygen, m)?)?;
    m.add_function(wrap_pyfunction!(certify, m)?)?;
    m.add_function(wrap_pyfunction!(verify, m)?)?;
    m.add_function(wrap_pyfunction!(version, m)?)?;
    Ok(())
}
