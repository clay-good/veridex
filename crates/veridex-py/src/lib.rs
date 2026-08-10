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

use veridex_core::adapter::{IngestOptions, Source};

/// Map a core ingest error to a Python `ValueError`.
fn to_py_err(e: impl std::fmt::Display) -> PyErr {
    PyValueError::new_err(e.to_string())
}

fn source_for(path: &str) -> Source {
    Source::Local(std::path::PathBuf::from(path))
}

/// `veridex.check(path, format=None) -> str`
///
/// Validate a dataset and return the report as a JSON string, byte-identical to
/// `veridex check --json`.
#[pyfunction]
#[pyo3(signature = (path, format=None))]
fn check(path: &str, format: Option<&str>) -> PyResult<String> {
    let registry = veridex_core::default_registry();
    let out = veridex_core::run_check(
        &registry,
        &source_for(path),
        format,
        &IngestOptions::default(),
    )
    .map_err(to_py_err)?;
    Ok(veridex_core::render_json(&out.verdict, Some(out.trust)))
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

/// `veridex.inspect(path, format=None) -> str`
///
/// Ingest a dataset and return its Canonical Dataset Model as a JSON string, identical to
/// `veridex inspect --json`. Runs no checks.
#[pyfunction]
#[pyo3(signature = (path, format=None))]
fn inspect(path: &str, format: Option<&str>) -> PyResult<String> {
    let registry = veridex_core::default_registry();
    let source = source_for(path);
    let opts = IngestOptions::default();
    let ingested = match format {
        Some(fmt) => registry.ingest_as(fmt, &source, &opts),
        None => registry.ingest(&source, &opts),
    }
    .map_err(to_py_err)?;
    serde_json::to_string_pretty(&ingested.dataset).map_err(to_py_err)
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
    m.add_function(wrap_pyfunction!(content_hash, m)?)?;
    m.add_function(wrap_pyfunction!(inspect, m)?)?;
    m.add_function(wrap_pyfunction!(catalog, m)?)?;
    m.add_function(wrap_pyfunction!(version, m)?)?;
    Ok(())
}
