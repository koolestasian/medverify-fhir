use crate::MedVerifyEngine;
use medverify_core::encoding::{
    encode_code_b36, encode_epoch_day, MueRecord, NcciPtpRecord, NO_DELETION_DATE,
};
use medverify_core::index::{write_mue, write_ptp, CatalogMode};
use pyo3::prelude::*;
use std::{fs::File, io, path::PathBuf};
use time::macros::date;

#[pyfunction]
pub fn _demo_engine(py: Python<'_>, index_dir: PathBuf) -> PyResult<MedVerifyEngine> {
    let effective = encode_epoch_day(date!(2026 - 10 - 01)).expect("demo date");
    let code = |code| encode_code_b36(code).expect("demo procedure code");
    let mode = CatalogMode::Synthetic;
    py.detach(|| -> io::Result<()> {
        write_ptp(
            File::create_new(index_dir.join("ncci_ptp.bin"))?,
            mode,
            vec![NcciPtpRecord {
                col1_b36: code("Y9001"),
                col2_b36: code("Y9002"),
                eff_days: effective,
                del_days: NO_DELETION_DATE,
                setting_flags: 1,
                ccmi: 1,
                flags: 0,
                rationale_id: 0,
            }],
        )?;
        write_mue(
            File::create_new(index_dir.join("ncci_mue.bin"))?,
            mode,
            vec![MueRecord {
                code_b36: code("Y9011"),
                eff_days: effective,
                del_days: NO_DELETION_DATE,
                max_units: 3,
                mai: 1,
                setting_flags: 1,
                flags: 0,
                reserved: 0,
                rationale_id: 0,
            }],
        )
    })?;
    MedVerifyEngine::new(py, index_dir)
}

#[pyfunction]
pub fn _demo_documents() -> (&'static str, &'static str) {
    (
        include_str!("../../medverify-core/tests/fixtures/pas/request.json"),
        include_str!("../../medverify-core/tests/fixtures/pas/response.json"),
    )
}
