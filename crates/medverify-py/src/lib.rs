mod findings;
mod line;

use findings::Findings;
use line::{ClaimLine, ModifierEvidence};
use medverify_core::claim::EvaluationError;
use medverify_core::index::{MmapMueIndex, MmapPtpIndex};
use medverify_core::{mue, ncci};
use pyo3::exceptions::{PyOverflowError, PyValueError};
use pyo3::prelude::*;
use rayon::prelude::*;
use std::io;
use std::path::PathBuf;

/// Evaluates claim lines against one immutable pair of NCCI indexes.
#[pyclass(frozen, module = "medverify._medverify")]
struct MedVerifyEngine {
    ptp: MmapPtpIndex,
    mue: MmapMueIndex,
}

/// Batches below this size run on the calling thread.
const PARALLEL_BATCH: usize = 64;

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
    module.add("__version__", env!("CARGO_PKG_VERSION"))
}
