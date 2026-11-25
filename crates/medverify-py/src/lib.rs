mod demo;
mod fhir_pas;
mod findings;
mod line;
mod wiser;

use findings::Findings;
use line::{ClaimLine, ModifierEvidence};
use medverify_core::claim::EvaluationError;
use medverify_core::index::{MmapMueIndex, MmapPtpIndex};
use medverify_core::receipt::{Receipt, MAX_PUBLIC_KEY_BYTES, MAX_RECEIPT_BYTES};
use medverify_core::{mue, ncci};
use pyo3::exceptions::{PyOverflowError, PyValueError};
use pyo3::prelude::*;
use rayon::prelude::*;
use serde::{de::DeserializeOwned, Serialize};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Evaluates claim lines against one immutable pair of NCCI indexes.
#[pyclass(frozen, module = "medverify._medverify")]
struct MedVerifyEngine {
    ptp: MmapPtpIndex,
    mue: MmapMueIndex,
}

/// Batches below this size run on the calling thread.
const PARALLEL_BATCH: usize = 64;

fn decode<T: DeserializeOwned>(text: &str, input: &str) -> PyResult<T> {
    serde_json::from_str(text).map_err(|error| {
        PyValueError::new_err(format!(
            "invalid {input} at line {}, column {}",
            error.line(),
            error.column()
        ))
    })
}

/// Parse a FHIR JSON document, rejecting repeated property names that a plain parse would merge.
fn decode_document(text: &str) -> PyResult<serde_json::Value> {
    medverify_core::fhir_pas::parse_json(text).map_err(|error| {
        PyValueError::new_err(format!(
            "invalid FHIR JSON at line {}, column {}{}",
            error.line(),
            error.column(),
            if error.is_data() {
                ": duplicate property"
            } else {
                ""
            }
        ))
    })
}

fn state(value: &str) -> PyResult<medverify_core::state::State> {
    value.parse().map_err(|_| {
        PyValueError::new_err("state must be an uppercase USPS state or territory code")
    })
}

fn extract<T: DeserializeOwned>(value: &Bound<'_, PyAny>, input: &str) -> PyResult<T> {
    let text: String = value
        .py()
        .import("json")?
        .call_method1("dumps", (value,))?
        .extract()?;
    decode(&text, input)
}

fn output<T: Serialize>(py: Python<'_>, value: T) -> PyResult<Bound<'_, PyAny>> {
    let json = serde_json::to_string(&value)
        .map_err(|_| pyo3::exceptions::PyRuntimeError::new_err("could not encode result"))?;
    py.import("json")?.call_method1("loads", (json,))
}

/// An evaluation failure reduced to plain data, so it can cross threads before becoming a `PyErr`.
struct Failure {
    overflow: bool,
    message: String,
}

impl Failure {
    fn new(error: EvaluationError<'_>) -> Self {
        Self {
            overflow: matches!(error, EvaluationError::UnitTotalOverflow { .. }),
            message: error.to_string(),
        }
    }

    fn into_err(self, prefix: Option<usize>) -> PyErr {
        let message = match prefix {
            Some(index) => format!("claims[{index}]: {}", self.message),
            None => self.message,
        };
        if self.overflow {
            PyOverflowError::new_err(message)
        } else {
            PyValueError::new_err(message)
        }
    }
}

impl MedVerifyEngine {
    fn findings_core(&self, claim: &[ClaimLine]) -> Result<Findings, Failure> {
        let lines: Vec<_> = claim.iter().map(ClaimLine::as_line).collect();
        let ptp = ncci::evaluate(&self.ptp, &lines).map_err(Failure::new)?;
        let mue = mue::evaluate(&self.mue, &lines).map_err(Failure::new)?;
        Ok(Findings {
            ptp: ptp.into_iter().map(Into::into).collect(),
            mue: mue.into_iter().map(Into::into).collect(),
        })
    }

    /// Findings in input order, or the error of the lowest failing index.
    fn batch(&self, claims: &[Vec<ClaimLine>]) -> Result<Vec<Findings>, (usize, Failure)> {
        if claims.len() < PARALLEL_BATCH {
            return claims
                .iter()
                .enumerate()
                .map(|(index, lines)| self.findings_core(lines).map_err(|e| (index, e)))
                .collect();
        }
        let results: Vec<_> = claims
            .par_iter()
            .map(|lines| self.findings_core(lines))
            .collect();
        results
            .into_iter()
            .enumerate()
            .map(|(index, result)| result.map_err(|failure| (index, failure)))
            .collect()
    }
}

#[pymethods]
impl MedVerifyEngine {
    #[new]
    fn new(py: Python<'_>, index_dir: PathBuf) -> PyResult<Self> {
        py.detach(move || {
            let ptp = MmapPtpIndex::open(index_dir.join("ncci_ptp.bin"))?;
            let mue = MmapMueIndex::open(index_dir.join("ncci_mue.bin"))?;
            if ptp.catalog_mode() != mue.catalog_mode() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "index catalogs differ",
                ));
            }
            Ok(Self { ptp, mue })
        })
        .map_err(PyErr::from)
    }

    /// Evaluate all lines in one group of claims, including service-day totals.
    fn evaluate(&self, py: Python<'_>, lines: Vec<ClaimLine>) -> PyResult<Findings> {
        py.detach(move || self.findings_core(&lines))
            .map_err(|failure| failure.into_err(None))
    }

    /// Evaluate independent groups of lines and return their findings in input order.
    fn evaluate_batch(
        &self,
        py: Python<'_>,
        claims: Vec<Vec<ClaimLine>>,
    ) -> PyResult<Vec<Findings>> {
        py.detach(move || self.batch(&claims))
            .map_err(|(index, failure)| failure.into_err(Some(index)))
    }
}

#[pymodule(gil_used = false)]
fn _medverify(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<MedVerifyEngine>()?;
    module.add_class::<ClaimLine>()?;
    module.add_class::<ModifierEvidence>()?;
    module.add_function(wrap_pyfunction!(demo::_demo_engine, module)?)?;
    module.add_function(wrap_pyfunction!(demo::_demo_documents, module)?)?;
    module.add_function(wrap_pyfunction!(_verify_receipt, module)?)?;
    wiser::register(module)?;
    fhir_pas::register(module)?;
    module.add("__version__", env!("CARGO_PKG_VERSION"))
}

fn read_bounded(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[pyfunction]
fn _verify_receipt(py: Python<'_>, file: PathBuf, public_key: PathBuf) -> PyResult<()> {
    py.detach(|| {
        let document = read_bounded(&file, MAX_RECEIPT_BYTES)
            .map_err(|_| PyValueError::new_err("could not read receipt file"))?;
        let key = read_bounded(&public_key, MAX_PUBLIC_KEY_BYTES)
            .map_err(|_| PyValueError::new_err("could not read public key file"))?;
        Receipt::verify_json(&document, &key)
            .map(|_| ())
            .map_err(|error| PyValueError::new_err(error.to_string()))
    })
}
