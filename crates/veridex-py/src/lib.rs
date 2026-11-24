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

/// The source a path argument names — the same rule the CLI applies, so `veridex.check("hf://…",
/// metadata_only=True)` behaves identically to `veridex check hf://… --metadata-only`.
///
/// A remote source Veridex cannot read is still refused by the core, by name; the point of drawing
/// the distinction here is that a remote reference must not be read as a relative path that happens
/// not to exist.
fn source_for(path: &str) -> Source {
    const REMOTE_SCHEMES: &[&str] = &["hf://", "http://", "https://", "s3://", "gs://"];
    if REMOTE_SCHEMES.iter().any(|s| path.starts_with(s)) {
        Source::Remote(path.to_string())
    } else {
        Source::Local(std::path::PathBuf::from(path))
    }
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
#[pyo3(signature = (path, format=None, config=None, sample_episodes=None, sample_fraction=None, sample_seed=0, metadata_only=false, profile=None, redact=false, attestation=None))]
#[allow(clippy::too_many_arguments)]
fn check(
    path: &str,
    format: Option<&str>,
    config: Option<&str>,
    sample_episodes: Option<u64>,
    sample_fraction: Option<f64>,
    sample_seed: u64,
    metadata_only: bool,
    profile: Option<&str>,
    redact: bool,
    attestation: Option<&str>,
) -> PyResult<String> {
    let registry = veridex_core::default_registry();
    let mut run_config = run_config_from(config)?;
    // `veridex check --profile <p>` had no Python equivalent at all, even though
    // `veridex.certify(profile=...)` did — so Python could *sign* a readiness certificate but not
    // produce the readiness report that justifies one.
    let profile = profile_by_name(profile)?;
    if let Some(p) = &profile {
        run_config.tolerances = p.apply_tolerances(run_config.tolerances);
    }
    let opts = ingest_options_from(sample_episodes, sample_fraction, sample_seed, metadata_only)?;
    // An attestation is verified against the dataset's own content hash, so the ingest happens here
    // and the verified result is handed to the same pipeline the CLI calls.
    let mut ingested = match format {
        Some(f) => registry.ingest_as(f, &source_for(path), &opts),
        None => registry.ingest(&source_for(path), &opts),
    }
    .map_err(to_py_err)?;
    ingested.dataset.canonicalize_order();
    let applied = match attestation {
        None => None,
        Some(document) => Some(applied_attestation(document, &ingested.dataset)?),
    };
    let attested_values: Vec<String> = match attestation {
        None => Vec::new(),
        Some(document) => serde_json::from_str::<veridex_core::SignedAttestation>(document)
            .map(|signed| {
                signed
                    .attestation
                    .elements
                    .iter()
                    .map(|e| e.value.clone())
                    .collect()
            })
            .unwrap_or_default(),
    };
    let out = veridex_core::check_ingested(ingested, &run_config, applied.as_ref());
    // Only a readiness profile has criteria; a threshold profile claims nothing about readiness.
    let readiness = profile.as_ref().filter(|p| p.judges_readiness()).map(|p| {
        veridex_core::certificate::ReadinessReport::evaluate(p, &out.verdict, &out.ingested.dataset)
    });
    // Which dataset the report is about, redacted exactly as the verdict is — see `redacted_id`.
    let dataset_id = redacted_id(redact, &out, &attested_values);
    Ok(veridex_core::render_json_with_readiness(
        &redacted_if(redact, &out, &attested_values),
        Some(out.trust),
        readiness.as_ref(),
        Some(&dataset_id),
    ))
}

/// The verdict to render: redacted for sharing, or the run's own.
///
/// `redact` is a rendering-time substitution — the verdict, the score, and the content hash are the
/// run's — so this is the only place the two paths differ.
fn redacted_if(
    redact: bool,
    out: &veridex_core::CheckOutput,
    attested_values: &[String],
) -> veridex_core::Verdict {
    if redact {
        veridex_core::Redactor::for_dataset(&out.ingested.dataset)
            .and_unread_sources(
                out.ingested
                    .report
                    .unread_sources
                    .iter()
                    .map(|u| u.source_path.as_str()),
            )
            // Attested values are not in the CDM, so the redactor cannot find them there — and the
            // conflict finding quotes them.
            .and_attested(attested_values.to_vec())
            .redact_verdict(&out.verdict)
    } else {
        out.verdict.clone()
    }
}

/// The dataset id a report should carry: the run's, or its placeholder under `--redact`.
///
/// Built from the same redactor as the verdict, so a shared report cannot carry the dataset's real
/// name in this field just because it is a new one.
fn redacted_id(
    redact: bool,
    out: &veridex_core::CheckOutput,
    attested_values: &[String],
) -> String {
    if redact {
        veridex_core::Redactor::for_dataset(&out.ingested.dataset)
            .and_unread_sources(
                out.ingested
                    .report
                    .unread_sources
                    .iter()
                    .map(|u| u.source_path.as_str()),
            )
            .and_attested(attested_values.to_vec())
            .redact_text(&out.ingested.dataset.id)
    } else {
        out.ingested.dataset.id.clone()
    }
}

/// Verify an attestation document against the dataset it claims to describe.
fn applied_attestation(
    document: &str,
    dataset: &veridex_core::cdm::Dataset,
) -> PyResult<veridex_core::AppliedAttestation> {
    let signed: veridex_core::SignedAttestation =
        serde_json::from_str(document).map_err(to_py_err)?;
    let hash = veridex_core::content_hash(dataset).to_hex();
    let producer_key = veridex_core::verify_attestation(&signed, &hash, None).map_err(to_py_err)?;
    Ok(veridex_core::AppliedAttestation {
        producer_key,
        keys: signed
            .attestation
            .elements
            .iter()
            .map(|e| e.key.clone())
            .collect(),
        conflicts: veridex_core::conflicts(dataset, &signed.attestation)
            .into_iter()
            .map(|c| {
                format!(
                    "{}: recorded `{}` → attested `{}`",
                    c.key, c.recorded, c.attested
                )
            })
            .collect(),
    })
}

/// Resolve a profile name, refusing an unknown one rather than silently ignoring it — a result that
/// claimed less than the caller asked for would be worse than an error.
fn profile_by_name(name: Option<&str>) -> PyResult<Option<veridex_core::profile::Profile>> {
    match name {
        None => Ok(None),
        Some(name) => Ok(Some(veridex_core::profile::by_name(name).ok_or_else(
            || {
                PyValueError::new_err(match veridex_core::profile::refusal_reason(name) {
                    Some(reason) => {
                        format!("`{name}` is not a profile Veridex provides: {reason}")
                    }
                    None => format!(
                        "unknown profile `{name}` (known: {})",
                        veridex_core::profile::KNOWN_PROFILES.join(", ")
                    ),
                })
            },
        )?)),
    }
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

    // `min_score` and `fail_on` are gate/exit-code fields the CLI reads off `CheckConfig` directly;
    // `to_run_config()` does not carry them, because there is no exit code to apply them to here.
    // Accepting them anyway meant a config whose whole purpose was to gate CI was parsed, validated
    // (a `min_score = 200` was even rejected!), and then discarded — `veridex.check(path,
    // config=open("veridex.toml").read())`, the migration path the README prescribes, returned a
    // clean result for a dataset the CLI refuses under the same file.
    //
    // Refused rather than silently honored: the caller gets the verdict as a return value, not an
    // exit status, so the gate is theirs to apply and the score is right there in the report. The
    // repo already treats a flag that is accepted and does nothing as a defect; this is that rule
    // reaching the Python front-end.
    let mut inert: Vec<&str> = Vec::new();
    if parsed.min_score.is_some() {
        inert.push("min_score");
    }
    if parsed.fail_on != veridex_core::config::FailOn::Error {
        inert.push("fail_on");
    }
    if !inert.is_empty() {
        return Err(pyo3::exceptions::PyValueError::new_err(format!(
            "config sets {}, which the Python API cannot apply: they gate the CLI's exit code, and \
             this call returns the report instead. Remove {} from the config you pass here and gate \
             on the returned report — `json.loads(veridex.check(...))[\"trust_score\"][\"score\"]` \
             and `[\"verdict\"][\"status\"]` carry what they would have decided.",
            inert.join(" and "),
            if inert.len() == 1 { "it" } else { "them" },
        )));
    }
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
    let mut ingested = ingested;
    ingested.dataset.canonicalize_order();
    // The shared core renderer, so this is byte-identical to `veridex inspect --json` by
    // construction rather than by two call sites happening to agree.
    Ok(veridex_core::render_inspect_json(&ingested))
}

/// `veridex.provenance(path, emit="croissant", format=None) -> str`
///
/// Extract the dataset's provenance and emit it as a JSON string — `croissant` (MLCommons Croissant
/// JSON-LD, the default) or `prov` (minimal W3C PROV) — byte-identical to `veridex provenance`.
#[pyfunction]
#[pyo3(signature = (path, emit, format=None, attestation=None))]
fn provenance(
    path: &str,
    emit: &str,
    format: Option<&str>,
    attestation: Option<&str>,
) -> PyResult<String> {
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
    // Attested provenance belongs in an emit: a document that omitted it would describe less than
    // the run did. Verified first, and the document names the key that signed it.
    let (attested, producer_key) = match attestation {
        None => (Vec::new(), String::new()),
        Some(document) => {
            let signed: veridex_core::SignedAttestation =
                serde_json::from_str(document).map_err(to_py_err)?;
            let hash = veridex_core::content_hash(&dataset).to_hex();
            let key = veridex_core::verify_attestation(&signed, &hash, None).map_err(to_py_err)?;
            (signed.attestation.elements, key)
        }
    };
    veridex_core::render_provenance_attested(&dataset, emit, &attested, &producer_key)
        .map_err(to_py_err)
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
    // Python strings are immutable and immortal until collected, so this copy cannot be scrubbed
    // once it crosses the boundary -- the caller owns that risk, as they do for any secret they
    // ask a library to hand back. The Rust-side copy is still scrubbed on drop.
    (kp.secret_hex().to_string(), kp.public_hex())
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
#[pyo3(signature = (path, secret_key_hex, timestamp=None, format=None, profile=None, config=None, attestation=None))]
fn certify(
    path: &str,
    secret_key_hex: &str,
    timestamp: Option<&str>,
    format: Option<&str>,
    profile: Option<&str>,
    config: Option<&str>,
    attestation: Option<&str>,
) -> PyResult<String> {
    let keypair = veridex_core::SigningKeypair::from_secret_hex(secret_key_hex)
        .ok_or_else(|| PyValueError::new_err("secret_key_hex is not a valid 32-byte hex key"))?;
    // An unknown profile name is an error, never a silently ignored argument that would produce a
    // certificate claiming less than the caller asked for.
    let profile = profile_by_name(profile)?;
    let registry = veridex_core::default_registry();
    // The config the caller passed, then the profile laid over it. This function used to take no
    // `config` at all while `check`, `check_sarif`, and `check_html` all did — and it is the one
    // that *signs*. The CLI's `certify` loads `--config`, or auto-discovers `./veridex.toml`, so
    // with a config present the two front-ends issued different signed documents for the same
    // dataset, key, and timestamp: a `disabled_checks` line produced `pass` from the CLI and `fail`
    // from Python, against this module's own promise that certificates are identical across them.
    let mut run_config = run_config_from(config)?;
    // A profile applies its own tolerances to the run (e.g. tighter cross-sensor sync) — through
    // `apply_tolerances`, which merges tighten-only, rather than by replacing the caller's whole
    // tolerance set with the profile's.
    if let Some(p) = &profile {
        run_config.tolerances = p.apply_tolerances(run_config.tolerances);
    }
    // Ingest once, verify any attestation against the resulting hash, then validate — the same
    // order the CLI uses, so the two issue the same document.
    let mut ingested = match format {
        Some(f) => registry.ingest_as(f, &source_for(path), &IngestOptions::default()),
        None => registry.ingest(&source_for(path), &IngestOptions::default()),
    }
    .map_err(to_py_err)?;
    ingested.dataset.canonicalize_order();
    let applied = match attestation {
        None => None,
        Some(document) => Some(applied_attestation(document, &ingested.dataset)?),
    };
    let out = veridex_core::check_ingested(ingested, &run_config, applied.as_ref());
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
        // Attestation-aware, or the certificate's coverage block would contradict its own trust
        // score.
        veridex_core::ProvenanceCoverage::of_with_attested(
            &out.ingested.dataset,
            &applied.as_ref().map(|a| a.keys.clone()).unwrap_or_default(),
        ),
        veridex_core::Issuance {
            key_id: keypair.public_hex(),
            timestamp: timestamp.map(String::from).unwrap_or_else(unix_timestamp),
        },
    );
    // Only a readiness profile has criteria to sign; a threshold profile's effect is already in the
    // certificate's effective config.
    if let Some(record) = &applied {
        cert.attestation = Some(veridex_core::certificate::document::AttestationRecord {
            producer_key: record.producer_key.clone(),
            keys: record.keys.clone(),
            timestamp: attestation
                .and_then(|d| serde_json::from_str::<veridex_core::SignedAttestation>(d).ok())
                .map(|signed| signed.attestation.timestamp)
                .unwrap_or_default(),
        });
    }
    if let Some(p) = profile.as_ref().filter(|p| p.judges_readiness()) {
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
        presented.is_some(),
    ))
}

/// `veridex.supersedes(earlier_certificate_json, later_certificate_json, public_key_hex=None,
/// allow_any_issuer=False) -> str`
///
/// Relate two certificates for one dataset and return the machine-readable summary
/// (`{relation, supersedes, reason, earlier, later, score_delta}`) as a JSON string, byte-identical
/// to `veridex supersedes --json`. Inputs are the certificate JSON strings, not file paths.
///
/// Both documents are verified first, under the same trust rule the CLI applies: a relation computed
/// from a document nobody verified is worth nothing, because the whole question is which signed
/// statement to trust. Raises `ValueError` when either does not verify, when no issuer is named and
/// `allow_any_issuer` is not set, or when either input is not a certificate.
#[pyfunction]
#[pyo3(signature = (earlier_certificate_json, later_certificate_json, public_key_hex=None, allow_any_issuer=false))]
fn supersedes(
    earlier_certificate_json: &str,
    later_certificate_json: &str,
    public_key_hex: Option<&str>,
    allow_any_issuer: bool,
) -> PyResult<String> {
    if public_key_hex.is_none() && !allow_any_issuer {
        return Err(PyValueError::new_err(
            "supersedes needs a trusted issuer: pass public_key_hex, or allow_any_issuer=True to \
             relate the two documents on whatever keys signed them",
        ));
    }
    let earlier: veridex_core::SignedCertificate =
        serde_json::from_str(earlier_certificate_json).map_err(to_py_err)?;
    let later: veridex_core::SignedCertificate =
        serde_json::from_str(later_certificate_json).map_err(to_py_err)?;
    for (label, signed) in [("earlier", &earlier), ("later", &later)] {
        veridex_core::verify(signed, None, public_key_hex).map_err(|e| {
            PyValueError::new_err(format!("the {label} certificate does not verify: {e}"))
        })?;
    }
    Ok(veridex_core::certificate::render_supersession_json(
        &veridex_core::certificate::relate(
            &earlier.certificate,
            &earlier.public_key,
            &later.certificate,
            &later.public_key,
        ),
    ))
}

/// `veridex.emit_cdm(path, format=None, metadata_only=False) -> str`
///
/// Read a dataset once and return its Canonical Dataset Model as a JSON string — the same document
/// `veridex check <dataset> --emit-cdm <file>` writes. Hand it to [`check_cdm`] to re-run the
/// catalog without reading the dataset again; nearly all of a run's cost is that read.
#[pyfunction]
#[pyo3(signature = (path, format=None, metadata_only=false))]
fn emit_cdm(path: &str, format: Option<&str>, metadata_only: bool) -> PyResult<String> {
    let registry = veridex_core::default_registry();
    let opts = IngestOptions {
        metadata_only,
        ..IngestOptions::default()
    };
    let source = source_for(path);
    let ingested = match format {
        Some(f) => registry.ingest_as(f, &source, &opts),
        None => registry.ingest(&source, &opts),
    }
    .map_err(to_py_err)?;
    Ok(veridex_core::cdm_cache::render(
        &veridex_core::cdm_cache::save(&ingested),
    ))
}

/// `veridex.explain(query) -> str`
///
/// What one finding code or check id is, as the JSON `veridex explain --json` prints: the check
/// behind it, whether the code is a measurement or an **abstention** ("I could not measure", which
/// produces the same silence as a clean result everywhere else), and — for the codes the engine
/// emits rather than a check — why no check owns it.
///
/// Returns the `{"found": false, "near": [...]}` shape for a string that is neither, rather than
/// raising: "that is not a code, did you mean one of these" is an answer.
#[pyfunction]
fn explain(query: &str) -> PyResult<String> {
    let engine = veridex_core::checks::default_engine().map_err(to_py_err)?;
    Ok(veridex_core::render_explain_json(&veridex_core::explain(
        &engine.catalog(),
        query,
    )))
}

/// `veridex.audit(log_text, expect_head=None, certificate_json=None) -> str`
///
/// Walk an append-only issuance log and return the JSON `veridex audit --json` prints: whether the
/// chain is intact, how many entries it holds, how many carried a verified signature, the chain
/// head, and the first break if there is one.
///
/// Takes the log's *text* rather than a path, like every other binding here, so a caller can audit a
/// log they fetched rather than one on this disk.
///
/// `expect_head` pins the head. Without it a log shortened from the end is a valid chain one entry
/// shorter, and nothing inside the file can say otherwise — the chain proves no past entry was
/// altered, not that none was dropped.
///
/// `certificate_json` asks the other question a log answers: does the issuer's own record name
/// **this certificate document**? Matched by digest over the bytes passed, because that is what an
/// entry holds — so a re-serialized certificate is reported as `{"recorded": false,
/// "same_dataset_entries": [...]}` rather than as a match or as an absence. When it is given,
/// `intact` folds the answer in, so one field is enough for a gate.
///
/// Raises `ValueError` when the text is not a Veridex audit log at all, which is a different answer
/// from a log that was edited.
#[pyfunction]
#[pyo3(signature = (log_text, expect_head=None, certificate_json=None))]
fn audit(
    log_text: &str,
    expect_head: Option<&str>,
    certificate_json: Option<&str>,
) -> PyResult<String> {
    let report = veridex_core::audit::verify_log(log_text).map_err(to_py_err)?;
    let rendered = veridex_core::audit::render_log_json(&report, expect_head);
    let Some(document) = certificate_json else {
        return Ok(rendered);
    };
    // The same answer, and the same folding into `intact`, as `veridex audit --certificate --json`:
    // one field is enough for a gate, and the detail is beside it.
    let mut doc: serde_json::Value =
        serde_json::from_str(&rendered).unwrap_or_else(|_| serde_json::json!({}));
    use veridex_core::audit::CertificateRecord::*;
    let record = veridex_core::audit::certificate_in_log(&report, document.as_bytes());
    doc["certificate"] = match &record {
        ThisDocument { seqs } => serde_json::json!({ "recorded": true, "entries": seqs }),
        SameDatasetOnly { seqs } => {
            serde_json::json!({ "recorded": false, "same_dataset_entries": seqs })
        }
        Absent => serde_json::json!({ "recorded": false }),
    };
    doc["intact"] = serde_json::json!(
        report.intact()
            && veridex_core::audit::head_mismatch(&report, expect_head).is_none()
            && matches!(record, ThisDocument { .. })
    );
    Ok(doc.to_string())
}

/// `veridex.check_cdm(cdm_json, config=None, profile=None, redact=False) -> str`
///
/// Validate a saved Canonical Dataset Model and return the report, byte-identical to
/// `veridex check --cdm <file> --json`. For the content hash the model names, this is the verdict a
/// full read produces — the model is the whole of what a read computes — plus one informational
/// finding saying the dataset was not opened.
///
/// Raises `ValueError` for a file that is not a model, one written under a different CDM encoding,
/// or one whose model no longer hashes to the hash it states.
#[pyfunction]
#[pyo3(signature = (cdm_json, config=None, profile=None, redact=false))]
fn check_cdm(
    cdm_json: &str,
    config: Option<&str>,
    profile: Option<&str>,
    redact: bool,
) -> PyResult<String> {
    let mut run_config = run_config_from(config)?;
    let profile = profile_by_name(profile)?;
    if let Some(p) = &profile {
        run_config.tolerances = p.apply_tolerances(run_config.tolerances);
    }
    let cache = veridex_core::cdm_cache::load(cdm_json)
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
    let from = veridex_core::CachedInput {
        veridex_version: cache.veridex_version.clone(),
        cdm_content_hash: cache.cdm_content_hash.clone(),
    };
    let out = veridex_core::check_ingested_cached(
        veridex_core::cdm_cache::as_ingested(cache),
        &run_config,
        None,
        veridex_core::RunControl::default(),
        Some(from),
    );
    let readiness = profile.as_ref().filter(|p| p.judges_readiness()).map(|p| {
        veridex_core::certificate::ReadinessReport::evaluate(p, &out.verdict, &out.ingested.dataset)
    });
    let dataset_id = redacted_id(redact, &out, &[]);
    Ok(veridex_core::render_json_with_readiness(
        &redacted_if(redact, &out, &[]),
        Some(out.trust),
        readiness.as_ref(),
        Some(&dataset_id),
    ))
}

/// `veridex.overlap(paths, format=None) -> str`
///
/// Compare two or more datasets and return the machine-readable summary
/// (`{schema, overlaps, shared_episodes, uncomparable_episodes, datasets, shared}`) as a JSON
/// string, byte-identical to `veridex overlap --json`. Inputs are dataset paths, as `check` takes.
///
/// The question `check` cannot answer, because every check looks inside one dataset: the same
/// recording, exported twice, arrives through two sources and is trained on twice. Exact matches
/// only — a re-export that recomputed its statistics is the same recording and is not reported.
///
/// Raises `ValueError` for fewer than two paths, or when any of them cannot be ingested.
#[pyfunction]
#[pyo3(signature = (paths, format=None))]
fn overlap(paths: Vec<String>, format: Option<&str>) -> PyResult<String> {
    if paths.len() < 2 {
        return Err(PyValueError::new_err(
            "overlap compares two or more datasets: pass at least two paths",
        ));
    }
    let registry = veridex_core::default_registry();
    let opts = IngestOptions::default();
    let mut ingested = Vec::with_capacity(paths.len());
    for path in &paths {
        let source = source_for(path);
        let one = match format {
            Some(fmt) => registry.ingest_as(fmt, &source, &opts),
            None => registry.ingest(&source, &opts),
        }
        .map_err(to_py_err)?;
        ingested.push((path.clone(), one));
    }
    let sources: Vec<veridex_core::overlap::Source<'_>> = ingested
        .iter()
        .map(|(name, i)| veridex_core::overlap::Source {
            name: name.clone(),
            dataset: &i.dataset,
        })
        .collect();
    Ok(veridex_core::overlap::render_overlap_json(
        &veridex_core::overlap::overlap(
            &sources,
            veridex_core::engine::Tolerances::default().near_duplicate_fraction,
        ),
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
#[pyo3(signature = (path, format=None, config=None, sample_episodes=None, sample_fraction=None, sample_seed=0, metadata_only=false, redact=false))]
#[allow(clippy::too_many_arguments)]
fn check_sarif(
    path: &str,
    format: Option<&str>,
    config: Option<&str>,
    sample_episodes: Option<u64>,
    sample_fraction: Option<f64>,
    sample_seed: u64,
    metadata_only: bool,
    redact: bool,
) -> PyResult<String> {
    let registry = veridex_core::default_registry();
    let run_config = run_config_from(config)?;
    // `veridex check --sarif` honors the sampling and metadata-only flags; this hardcoded
    // `IngestOptions::default()`, so a Python CI pipeline could not produce SARIF or HTML for a
    // partial run at all — and the coverage finding those runs emit had nowhere to appear.
    let opts = ingest_options_from(sample_episodes, sample_fraction, sample_seed, metadata_only)?;
    let out =
        veridex_core::run_check_with(&registry, &source_for(path), format, &opts, &run_config)
            .map_err(to_py_err)?;
    Ok(
        serde_json::to_string_pretty(&veridex_core::render_sarif(&redacted_if(redact, &out, &[])))
            .expect("sarif serializes"),
    )
}

/// `veridex.check_html(path, format=None, config=None) -> str`
///
/// Validate a dataset and return the self-contained HTML report, byte-identical to
/// `veridex check --html`.
#[pyfunction]
#[pyo3(signature = (path, format=None, config=None, sample_episodes=None, sample_fraction=None, sample_seed=0, metadata_only=false, redact=false))]
#[allow(clippy::too_many_arguments)]
fn check_html(
    path: &str,
    format: Option<&str>,
    config: Option<&str>,
    sample_episodes: Option<u64>,
    sample_fraction: Option<f64>,
    sample_seed: u64,
    metadata_only: bool,
    redact: bool,
) -> PyResult<String> {
    let registry = veridex_core::default_registry();
    let run_config = run_config_from(config)?;
    let opts = ingest_options_from(sample_episodes, sample_fraction, sample_seed, metadata_only)?;
    let out =
        veridex_core::run_check_with(&registry, &source_for(path), format, &opts, &run_config)
            .map_err(to_py_err)?;
    Ok(veridex_core::render_html(
        &redacted_if(redact, &out, &[]),
        Some(out.trust),
    ))
}

/// `veridex.effective_config(config=None, profile=None, min_score=None, fail_on=None) -> str`
///
/// The effective merged configuration as JSON (`veridex.config/1`): every setting's resolved value
/// and the layer it came from — built-in default, config file, policy profile, or an explicit
/// argument. The same document the CLI's `veridex check --print-config --json` prints.
///
/// Unlike [`check`], this accepts a config that sets `min_score` / `fail_on`: reporting what a
/// config *says* is the whole point of the call, and nothing here acts on them.
#[pyfunction]
#[pyo3(signature = (config=None, profile=None, min_score=None, fail_on=None))]
fn effective_config(
    config: Option<&str>,
    profile: Option<&str>,
    min_score: Option<u8>,
    fail_on: Option<&str>,
) -> PyResult<String> {
    let parsed = match config {
        None => veridex_core::CheckConfig::default(),
        Some(text) => {
            let parsed =
                veridex_core::CheckConfig::from_toml(text).map_err(|e| to_py_err(e.to_string()))?;
            let engine =
                veridex_core::checks::default_engine().map_err(|e| to_py_err(e.to_string()))?;
            parsed
                .validate_check_ids(engine.check_ids())
                .map_err(|e| to_py_err(e.to_string()))?;
            parsed
        }
    };
    let profile = profile_by_name(profile)?;
    let fail_on_flag = match fail_on {
        None => None,
        Some("error") => Some(veridex_core::FailOn::Error),
        Some("warning") => Some(veridex_core::FailOn::Warning),
        Some(v) => {
            return Err(PyValueError::new_err(format!(
                "invalid fail_on `{v}` (expected `error` or `warning`)"
            )))
        }
    };
    if let Some(n) = min_score {
        if n > 100 {
            return Err(PyValueError::new_err(format!(
                "min_score must be between 0 and 100, got {n}"
            )));
        }
    }
    let mut tolerances = parsed.to_run_config().tolerances;
    if let Some(p) = &profile {
        tolerances = p.apply_tolerances(tolerances);
    }
    let from_env = std::collections::BTreeSet::new();
    let inputs = veridex_core::effective::Inputs {
        // Python is handed config *text*, not a path: there is no file to name.
        config_path: None,
        file: &parsed,
        // The `VERIDEX_*` environment is a CLI input-gathering mechanism. An imported library that
        // silently reconfigured itself from the process environment would change what
        // `veridex.check(...)` means without the caller writing anything.
        from_env: &from_env,
        profile: profile.as_ref(),
        tolerances,
        fail_on: fail_on_flag.unwrap_or(parsed.fail_on),
        fail_on_from_flag: fail_on_flag.is_some(),
        min_score: min_score.or(parsed.min_score),
        min_score_from_flag: min_score.is_some(),
    };
    Ok(veridex_core::render_effective_config_json(&inputs))
}

/// `veridex.label(certificate_json, public_key_hex=None, allow_any_issuer=False) -> str`
///
/// Render a signed certificate as a Markdown trust label for a dataset card — the same document
/// `veridex label` prints.
///
/// The signature is verified first: a label rendered from a certificate that does not verify is
/// exactly the artifact an attacker wants, a paste-ready grade with no provenance behind it. A trust
/// decision about the issuer is required rather than defaulted, and when it is `allow_any_issuer`
/// the label itself says the issuer is unverified — that caveat has to survive being pasted.
#[pyfunction]
#[pyo3(signature = (certificate_json, public_key_hex=None, allow_any_issuer=false))]
fn label(
    certificate_json: &str,
    public_key_hex: Option<&str>,
    allow_any_issuer: bool,
) -> PyResult<String> {
    let signed: veridex_core::SignedCertificate =
        serde_json::from_str(certificate_json).map_err(to_py_err)?;
    if public_key_hex.is_none() && !allow_any_issuer {
        return Err(PyValueError::new_err(
            "label needs a trusted issuer: pass public_key_hex, or allow_any_issuer=True to render \
             a label that says on its face that the issuer is unverified",
        ));
    }
    veridex_core::verify(&signed, None, public_key_hex).map_err(to_py_err)?;
    Ok(veridex_core::render_label(
        &signed,
        public_key_hex.is_some(),
    ))
}

/// `veridex.attest(path, secret_key_hex, elements, timestamp=None, format=None) -> str`
///
/// Sign provenance a producer can vouch for, bound to this dataset's CDM content hash. `elements` is
/// a `{key: value}` mapping over the provenance keys Veridex scores. Returns the signed attestation
/// document as JSON — the same document `veridex attest` writes.
#[pyfunction]
#[pyo3(signature = (path, secret_key_hex, elements, timestamp=None, format=None))]
fn attest(
    path: &str,
    secret_key_hex: &str,
    elements: std::collections::BTreeMap<String, String>,
    timestamp: Option<&str>,
    format: Option<&str>,
) -> PyResult<String> {
    let keypair = veridex_core::SigningKeypair::from_secret_hex(secret_key_hex)
        .ok_or_else(|| PyValueError::new_err("secret_key_hex is not a valid 32-byte hex key"))?;
    if elements.is_empty() {
        return Err(PyValueError::new_err(
            "attest needs at least one provenance element",
        ));
    }
    for key in elements.keys() {
        if !veridex_core::certificate::EXPECTED_PROVENANCE_KEYS.contains(&key.as_str()) {
            return Err(PyValueError::new_err(format!(
                "`{key}` is not a provenance element Veridex scores (expected: {})",
                veridex_core::certificate::EXPECTED_PROVENANCE_KEYS.join(", ")
            )));
        }
    }
    let registry = veridex_core::default_registry();
    let mut ingested = match format {
        Some(f) => registry.ingest_as(f, &source_for(path), &IngestOptions::default()),
        None => registry.ingest(&source_for(path), &IngestOptions::default()),
    }
    .map_err(to_py_err)?;
    ingested.dataset.canonicalize_order();
    let attestation = veridex_core::Attestation::build(
        ingested.dataset.id.clone(),
        veridex_core::content_hash(&ingested.dataset).to_hex(),
        elements,
        timestamp.map(String::from).unwrap_or_else(unix_timestamp),
    )
    .map_err(PyValueError::new_err)?;
    let signed = veridex_core::sign_attestation(attestation, &keypair);
    serde_json::to_string_pretty(&signed).map_err(to_py_err)
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
    m.add(
        "PLUGIN_API_VERSION",
        veridex_core::engine::PLUGIN_API_VERSION,
    )?;
    m.add_class::<PyCheckPack>()?;
    m.add_function(wrap_pyfunction!(check_with_packs, m)?)?;
    m.add_function(wrap_pyfunction!(check, m)?)?;
    m.add_function(wrap_pyfunction!(check_sarif, m)?)?;
    m.add_function(wrap_pyfunction!(check_html, m)?)?;
    m.add_function(wrap_pyfunction!(content_hash, m)?)?;
    m.add_function(wrap_pyfunction!(inspect, m)?)?;
    m.add_function(wrap_pyfunction!(catalog, m)?)?;
    m.add_function(wrap_pyfunction!(provenance, m)?)?;
    m.add_function(wrap_pyfunction!(diff, m)?)?;
    m.add_function(wrap_pyfunction!(effective_config, m)?)?;
    m.add_function(wrap_pyfunction!(label, m)?)?;
    m.add_function(wrap_pyfunction!(attest, m)?)?;
    m.add_function(wrap_pyfunction!(keygen, m)?)?;
    m.add_function(wrap_pyfunction!(certify, m)?)?;
    m.add_function(wrap_pyfunction!(verify, m)?)?;
    m.add_function(wrap_pyfunction!(supersedes, m)?)?;
    m.add_function(wrap_pyfunction!(overlap, m)?)?;
    m.add_function(wrap_pyfunction!(emit_cdm, m)?)?;
    m.add_function(wrap_pyfunction!(check_cdm, m)?)?;
    m.add_function(wrap_pyfunction!(audit, m)?)?;
    m.add_function(wrap_pyfunction!(explain, m)?)?;
    m.add_function(wrap_pyfunction!(version, m)?)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Check-packs from Python: a lab's own rules, under the same accountability
// ---------------------------------------------------------------------------

/// One check a Python caller defined: its declared identity plus the callable that runs it.
struct PythonCheck {
    /// Leaked once at construction. The `Check` trait takes `&'static str` because a built-in
    /// check's id is a literal; a Python pack's ids arrive at runtime, and leaking them is the
    /// honest cost of letting a runtime string be a check id. A pack is built once per process.
    id: &'static str,
    title: &'static str,
    version: &'static str,
    codes: &'static [&'static str],
    category: veridex_core::Category,
    severity: veridex_core::Severity,
    scope: veridex_core::Scope,
    /// The Python callable. Takes the CDM as a JSON string and returns a list of findings.
    run: Py<PyAny>,
}

/// The findings a Python check returned, as declared in its own vocabulary.
#[derive(serde::Deserialize)]
struct PyFinding {
    code: String,
    message: String,
    #[serde(default)]
    severity: Option<String>,
    #[serde(default)]
    episode: Option<u64>,
    #[serde(default)]
    stream: Option<String>,
    #[serde(default)]
    risk: Option<String>,
    #[serde(default)]
    remedy: Option<String>,
}

impl veridex_core::Check for PythonCheck {
    fn id(&self) -> &'static str {
        self.id
    }
    fn title(&self) -> &'static str {
        self.title
    }
    fn category(&self) -> veridex_core::Category {
        self.category
    }
    fn default_severity(&self) -> veridex_core::Severity {
        self.severity
    }
    fn scope(&self) -> veridex_core::Scope {
        self.scope
    }
    fn version(&self) -> &'static str {
        self.version
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        self.codes
    }
    fn run(&self, dataset: &veridex_core::cdm::Dataset) -> Vec<veridex_core::check::Finding> {
        // The CDM as JSON: the same model every built-in check reads, in the one shape a Python
        // caller can already work with. Serializing per check costs a copy of a few megabytes and
        // buys a plugin surface that needs no binding for every CDM type.
        let cdm = serde_json::to_string(dataset).expect("the CDM serializes");
        // A Python exception, a malformed return, or a code the check never declared all become a
        // panic here, which the engine's fault isolation turns into an `errored` check: the run
        // continues, and the verdict says this check failed to run rather than that it found
        // nothing. Dropping to "no findings" is the one outcome a plugin surface must never have.
        let findings: Vec<PyFinding> = Python::attach(|py| {
            let returned = self
                .run
                .call1(py, (cdm,))
                .unwrap_or_else(|e| panic!("check `{}` raised: {e}", self.id));
            let json: String = py
                .import("json")
                .and_then(|m| m.call_method1("dumps", (returned,)))
                .and_then(|s| s.extract())
                .unwrap_or_else(|e| {
                    panic!(
                        "check `{}` returned something that is not JSON-serializable: {e}",
                        self.id
                    )
                });
            serde_json::from_str(&json).unwrap_or_else(|e| {
                panic!(
                    "check `{}` must return a list of {{code, message, …}} objects: {e}",
                    self.id
                )
            })
        });
        findings
            .into_iter()
            .map(|f| {
                assert!(
                    self.codes.contains(&f.code.as_str()),
                    "check `{}` emitted `{}`, which it does not declare — a finding code that is \
                     not in the catalog is one no report can explain",
                    self.id,
                    f.code
                );
                let severity = match f.severity.as_deref() {
                    None => self.severity,
                    Some("error") => veridex_core::Severity::Error,
                    Some("warning") => veridex_core::Severity::Warning,
                    Some("info") => veridex_core::Severity::Info,
                    Some(other) => panic!(
                        "check `{}` returned severity `{other}`: expected error, warning or info",
                        self.id
                    ),
                };
                let location = match (f.episode, f.stream) {
                    (Some(episode), Some(stream)) => {
                        veridex_core::Location::Stream { episode, stream }
                    }
                    (Some(episode), None) => veridex_core::Location::Episode { episode },
                    (None, Some(_)) => panic!(
                        "check `{}` named a stream without an episode: a stream location is an \
                         episode's stream",
                        self.id
                    ),
                    (None, None) => veridex_core::Location::Dataset,
                };
                let mut finding = veridex_core::check::Finding::new(
                    self.id,
                    self.category,
                    severity,
                    location,
                    // Leaked for the same reason the id is: a finding's code is `&'static str`
                    // because the catalog's codes are literals.
                    Box::leak(f.code.into_boxed_str()),
                    f.message,
                );
                if let Some(risk) = f.risk {
                    finding = finding.with_risk(risk);
                }
                if let Some(remedy) = f.remedy {
                    finding = finding.with_remedy(remedy);
                }
                finding
            })
            .collect()
    }
}

/// `veridex.CheckPack(name, version, api_version=veridex.PLUGIN_API_VERSION)`
///
/// A named, versioned set of checks a lab, a hardware vendor or a benchmark maintainer writes for
/// their own data — the same surface Rust callers get from `veridex_core::engine::CheckPack`, and
/// under the same accountability. Every one of a pack's check ids sits under its name
/// (`mylab/house-rule`), the verdict records the pack's name and version, and that record is part of
/// the result content hash, so a run under a larger catalog says so rather than looking like a
/// disagreement about the data.
///
/// ```python
/// pack = veridex.CheckPack("mylab", "1.0.0")
/// pack.add_check(
///     id="mylab/gripper-recorded",
///     title="The gripper stream is present",
///     category="structural",
///     severity="error",
///     codes=["MYLAB.NO_GRIPPER"],
///     run=lambda cdm: ([] if "gripper" in cdm else
///                      [{"code": "MYLAB.NO_GRIPPER", "message": "no gripper stream"}]),
/// )
/// report = veridex.check("dataset/", packs=[pack])
/// ```
///
/// The callable is handed the CDM as a **JSON string** and returns a list of
/// `{"code", "message"}` objects, optionally with `severity`, `episode`, `stream`, `risk` and
/// `remedy`. A check that raises, returns something else, or emits a code it did not declare is
/// recorded in the verdict as `errored` — the run continues and says the check failed to run, rather
/// than reading as a check that found nothing.
///
/// What a Python pack cannot do is what a Rust pack cannot do: sign anything. A check returns
/// findings; signing is reachable only from the issuer's own key through `veridex.certify`.
#[pyclass(name = "CheckPack")]
struct PyCheckPack {
    name: String,
    version: String,
    api_version: String,
    checks: Vec<PythonCheckSpec>,
}

/// One `add_check` call, kept until the pack is handed to a run.
struct PythonCheckSpec {
    id: String,
    title: String,
    version: String,
    codes: Vec<String>,
    category: String,
    severity: String,
    scope: String,
    run: Py<PyAny>,
}

#[pymethods]
impl PyCheckPack {
    #[new]
    #[pyo3(signature = (name, version, api_version=None))]
    fn new(name: &str, version: &str, api_version: Option<&str>) -> Self {
        PyCheckPack {
            name: name.to_string(),
            version: version.to_string(),
            api_version: api_version
                .unwrap_or(veridex_core::engine::PLUGIN_API_VERSION)
                .to_string(),
            checks: Vec::new(),
        }
    }

    /// Add a check to this pack. `category` is one of the catalog's seven families, `severity` is
    /// the check's default (`error`, `warning`, `info`), and `scope` is `dataset`, `episode` or
    /// `stream`.
    #[pyo3(signature = (id, title, run, codes, category, severity="warning", scope="dataset", version="1"))]
    #[allow(clippy::too_many_arguments)]
    fn add_check(
        &mut self,
        id: &str,
        title: &str,
        run: Py<PyAny>,
        codes: Vec<String>,
        category: &str,
        severity: &str,
        scope: &str,
        version: &str,
    ) -> PyResult<()> {
        if codes.is_empty() {
            return Err(PyValueError::new_err(format!(
                "check `{id}` declares no finding codes: a check that can emit a finding no report \
                 can explain is not a check"
            )));
        }
        self.checks.push(PythonCheckSpec {
            id: id.to_string(),
            title: title.to_string(),
            version: version.to_string(),
            codes,
            category: category.to_string(),
            severity: severity.to_string(),
            scope: scope.to_string(),
            run,
        });
        Ok(())
    }

    /// The pack's name, which is the namespace every one of its check ids sits under.
    #[getter]
    fn name(&self) -> &str {
        &self.name
    }

    /// The pack's version, as recorded in the verdict.
    #[getter]
    fn version(&self) -> &str {
        &self.version
    }

    /// The plugin API this pack declares it was written against.
    #[getter]
    fn api_version(&self) -> &str {
        &self.api_version
    }
}

fn category_by_name(name: &str) -> PyResult<veridex_core::Category> {
    Ok(match name {
        "structural" => veridex_core::Category::Structural,
        "temporal" => veridex_core::Category::Temporal,
        "statistical" => veridex_core::Category::Statistical,
        "semantic" => veridex_core::Category::Semantic,
        "video" => veridex_core::Category::Video,
        "provenance" => veridex_core::Category::Provenance,
        "autonomy" => veridex_core::Category::Autonomy,
        other => {
            return Err(PyValueError::new_err(format!(
                "unknown category `{other}`: a check belongs to exactly one of structural, \
                 temporal, statistical, semantic, video, provenance, autonomy"
            )))
        }
    })
}

fn severity_by_name(name: &str) -> PyResult<veridex_core::Severity> {
    Ok(match name {
        "error" => veridex_core::Severity::Error,
        "warning" => veridex_core::Severity::Warning,
        "info" => veridex_core::Severity::Info,
        other => {
            return Err(PyValueError::new_err(format!(
                "unknown severity `{other}`: expected error, warning or info"
            )))
        }
    })
}

fn scope_by_name(name: &str) -> PyResult<veridex_core::Scope> {
    Ok(match name {
        "dataset" => veridex_core::Scope::Dataset,
        "episode" => veridex_core::Scope::Episode,
        "stream" => veridex_core::Scope::Stream,
        other => {
            return Err(PyValueError::new_err(format!(
                "unknown scope `{other}`: expected dataset, episode or stream"
            )))
        }
    })
}

/// Turn the Python packs a caller passed into core packs, refusing anything the core refuses.
///
/// Every refusal is the core's own — an incompatible plugin API, a name that cannot be a namespace,
/// a check id that does not sit under it — so the two surfaces cannot disagree about what a valid
/// pack is.
fn core_packs(
    packs: Vec<PyRef<'_, PyCheckPack>>,
) -> PyResult<Vec<veridex_core::engine::CheckPack>> {
    let mut out = Vec::new();
    for pack in packs {
        let name: &'static str = Box::leak(pack.name.clone().into_boxed_str());
        let version: &'static str = Box::leak(pack.version.clone().into_boxed_str());
        let mut checks: Vec<Box<dyn veridex_core::Check>> = Vec::new();
        for spec in &pack.checks {
            let codes: Vec<&'static str> = spec
                .codes
                .iter()
                .map(|c| &*Box::leak(c.clone().into_boxed_str()))
                .collect();
            checks.push(Box::new(PythonCheck {
                id: Box::leak(spec.id.clone().into_boxed_str()),
                title: Box::leak(spec.title.clone().into_boxed_str()),
                version: Box::leak(spec.version.clone().into_boxed_str()),
                codes: Box::leak(codes.into_boxed_slice()),
                category: category_by_name(&spec.category)?,
                severity: severity_by_name(&spec.severity)?,
                scope: scope_by_name(&spec.scope)?,
                run: Python::attach(|py| spec.run.clone_ref(py)),
            }));
        }
        out.push(
            veridex_core::engine::CheckPack::for_api(&pack.api_version, name, version, checks)
                .map_err(to_py_err)?,
        );
    }
    Ok(out)
}

/// `veridex.check_with_packs(path, packs, format=None, config=None, metadata_only=False) -> str`
///
/// As `veridex.check`, with third-party check-packs loaded **beside** the built-in catalog — never
/// in place of it, which is what makes a report from one Veridex a report from every Veridex.
///
/// The verdict records each pack's name and version, and that record is inside the result content
/// hash: the same dataset checked under a larger catalog is a different result and says so. A
/// certificate issued from such a run names the packs too, because an offline reader cannot re-run
/// Veridex to discover that "pass" was a pass under a catalog somebody extended.
#[pyfunction]
#[pyo3(signature = (path, packs, format=None, config=None, metadata_only=false))]
fn check_with_packs(
    path: &str,
    packs: Vec<PyRef<'_, PyCheckPack>>,
    format: Option<&str>,
    config: Option<&str>,
    metadata_only: bool,
) -> PyResult<String> {
    let packs = core_packs(packs)?;
    let registry = veridex_core::default_registry();
    let run_config = run_config_from(config)?;
    let opts = ingest_options_from(None, None, 0, metadata_only)?;
    let mut ingested = match format {
        Some(f) => registry.ingest_as(f, &source_for(path), &opts),
        None => registry.ingest(&source_for(path), &opts),
    }
    .map_err(to_py_err)?;
    ingested.dataset.canonicalize_order();
    let out = veridex_core::pipeline::check_ingested_with_packs(
        ingested,
        &run_config,
        None,
        veridex_core::RunControl::default(),
        None,
        packs,
    );
    let dataset_id = out.ingested.dataset.id.clone();
    Ok(veridex_core::render_json_with_readiness(
        &out.verdict,
        Some(out.trust),
        None,
        Some(&dataset_id),
    ))
}
