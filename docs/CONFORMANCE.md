# Conformance

Results of the official Inferno server test suites against the sandbox payer servers, written by
`scripts/run_inferno.py` from an actual local run. The suites exercise the Nirmitee Go CRD, DTR and
PAS servers built from the commits below; they do not exercise MedVerify's Rust PAS validator, which
has its own gate in `cargo test` (crates/medverify-core/tests/pas.rs). The suites reach each server
over TLS signed by a sandbox test CA. Passing Inferno is a conformance signal for these pinned
builds, not a certification.

Overall verdict: **pass**. Run finished 20261125T044947Z (UTC). Detailed reports: `artifacts/inferno/20261125T042740Z/`.

A suite passes only when every test and group has a known verdict that is pass, omit (does not
apply), or a skip of something the kit marks optional. A missing or unknown verdict, failure,
error, cancellation, timeout, unresolved wait or required skip fails it.

## Candidate

| Item | Value |
|---|---|
| Base commit | cd8a7d217c7ef21abe2fe25619cb70cb83556619 |
| Working tree | clean |
| Files | 94 |
| Content digest | 47624fcfdac3fd4ed7a5149222865040ec8263eb906206f37462b03a9f7cc7ab |

## Results

| Suite | IG | Kit | Server commit | Tests | Pass | Omit | Skip | Fail | Error | Other | Groups | Verdict |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| crd_server_v221 | 2.2.1 | v0.15.0 | fd2b148323d4 | 143 | 136 | 0 | 7 | 0 | 0 | 0 | 48 | pass |
| dtr_payer_server_v220 | 2.2.0 | v0.18.1 | 566b2243fe18 | 46 | 45 | 1 | 0 | 0 | 0 | 0 | 15 | pass |
| davinci_pas_server_suite_v221 | 2.2.1 | v0.16.0 | 2ad7923f745e | 84 | 84 | 0 | 0 | 0 | 0 | 0 | 26 | pass |

### Da Vinci CRD Server details

- Optional, skipped: 3.2.3.03 Launch SMART Application cards are valid
- Optional, skipped: 3.2.3.07 Request Form Completion cards and system actions are valid
- Optional, skipped: 3.3.3.03 Launch SMART Application cards are valid
- Optional, skipped: 3.3.3.07 Propose Alternate Request cards are valid
- Optional, skipped: 3.3.3.09 Request Form Completion cards and system actions are valid
- Optional, skipped: 3.6.3.03 Launch SMART Application cards are valid
- Optional, skipped: 3.6.3.07 Request Form Completion cards and system actions are valid

### Da Vinci DTR Payer Server details

- Omitted (does not apply): 3.4.06 Contained Binary resources are PDFs or safe XHTML

## Timings

| Step | Seconds |
|---|---|
| Seed: synthea | 28.9 |
| Seed: upload | 35.0 |
| Seed: fingerprint | 9.1 |
| Reset cycle 1 | 36.5 |
| Reset cycle 2 | 34.1 |
| CRD group 1 | 9.0 |
| CRD group 3 | 260.3 |
| CRD group 2 | 3.0 |
| CRD group 4 | 3.0 |
| DTR group 1 | 322.4 |
| DTR group 2 | 3.0 |
| DTR group 3 | 9.1 |
| DTR group 4 | 3.0 |
| PAS group 1 | 310.2 |
| PAS group 2 | 36.2 |
| PAS group 3 | 232.5 |
| PAS group 4 | 3.1 |

## Hardware

| Item | Value |
|---|---|
| CPU | AMD Ryzen 9 7900X 12-Core Processor |
| Docker engine CPUs | 24 |
| Docker engine memory | 30.9 GiB |
| Docker engine OS | Ubuntu 24.04.5 LTS |
| Kernel | 6.18.58.1-microsoft-standard-WSL2 |
| Docker | 29.1.3 |
| Runner OS | Windows-11-10.0.26300-SP0 |

## Seed data

50 synthetic Washington patients from Synthea v4.0.0, seed 42, clinician seed 20260101, simulation date 20261101.
Server fingerprint `72783ae5ac9f8fa9400b6c017baa340d96f9bac692673df7fe3cfbc91a2f00ed`.
2 reset cycles each removed planted mutations and restored that fingerprint and 50 patients.
