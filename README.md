# MedVerify-FHIR

MedVerify-FHIR checks Medicare coding edits, Washington WISeR policy, and Da Vinci Prior Authorization Support (PAS) transactions. All three are available in Rust and through the typed Python API, which is tested on CPython 3.14 and free-threaded 3.14t.

- **NCCI** evaluates procedure-to-procedure (PTP) and medically unlikely edits (MUE) against immutable indexes. The included dataset has 120 synthetic procedures.
- **Washington WISeR** checks prior authorization and pre-payment review routing, epidural injection and cervical fusion criteria, tracking numbers, and provider exemptions. The bundled policy uses synthetic procedure mappings. Load an authorized table for actual codes with `wiser::Policy::from_toml`. [Bundled policy](policy/wa_wiser.toml)
- **Da Vinci PAS 2.2.1** validates FHIR R4 request and response Bundles, internal references, line decisions, and subscription notifications. The Rust core also evaluates CMS-0057-F deadlines and dated reviewer requirements for Arizona, California, Maryland, Texas, and Washington. [Reviewer rules](policy/state_hitl.toml)

## Quickstart

After installing the wheel, run `medverify demo` from any directory. It runs four synthetic checks offline, verifies a repaired control for each, and prints the measured check times.

From the repository root, use stable Rust with a native linker, CPython 3.11 or later, and uv installed as a Python module. Run these commands in Bash, or Git Bash on Windows, to build the synthetic indexes and check one line against its unit limit.

```sh
cargo run --locked -p medverify-core --bin medverify-index -- synthetic --data data --out target/synthetic
python -m uv run --locked python - <<'PY'
from medverify import ClaimLine, MedVerifyEngine

engine = MedVerifyEngine("target/synthetic")
line = ClaimLine(
    id=1, beneficiary="patient-a", billing_provider="provider-a",
    date="2026-10-01", setting="practitioner", code="Y9011", units=4,
)
finding = engine.evaluate([line])["mue"][0]
print(f"{finding['code']}: {finding['total_units']} billed units, limit {finding['max_units']}")
PY
```

Include lines that share a beneficiary, billing provider, service date, and setting in one `evaluate` call. MUE adjudication indicators (MAI) 2 and 3 total units for each code across those lines, including lines on separate claims. Use `evaluate_batch` for independent groups. Batches of 64 or more groups run in parallel. Results stay in input order, and evaluation errors report the lowest failing input index. Threads can share one engine.

MAI 1 applies the unit limit to each line. Pass `claim_id` when the claim is known. Within that claim, the evaluator allows one ordinary line per code and checks additional lines for an applicable billed modifier and matching, line-specific `ModifierEvidence`. Repeat modifiers (`76`, `77`, `91`) must trace to an original service and cannot support E/M lines. Distinct-service and anatomic modifiers must document which services were separate.

The evaluator reports `unsupported_split` for an unsupported additional line on a known claim and `possible_unsupported_split` when an unsupported line may be part of a split and its claim ID is unknown. Both are coding-review findings. A line over its own unit limit receives a separate `claim_line` finding.

Rust callers can open the generated files with `MmapPtpIndex::open` and `MmapMueIndex::open` and pass `claim::ClaimLine` records to `ncci::evaluate` and `mue::evaluate`. `write_ptp` produces format version 2, which adds a directory of column-one codes so a lookup searches only that code's rows. Version 1 PTP files still open.

## Python policy API

`medverify.wiser.Policy.washington_synthetic()` loads the bundled Washington policy. Use `Policy.from_toml(text)` with authorized procedure mappings. Pass typed service and stage dictionaries to `policy.route`. The same policy exposes gold-card thresholds, eligibility, and associated denials. The module also provides `evaluate_esi`, `evaluate_fusion`, and `validate_utn`.

`medverify.fhir_pas.validate_request(document)` takes FHIR R4 JSON text and returns an owned request handle. Pass that handle to `validate_response(document, request)` to correlate a response. A validation failure raises `ValueError` with its PAS validation code. Documents that repeat a property name in any object are rejected, as FHIR R4 JSON requires. Use `Timeline` for receipt and notice deadlines and `ReviewerPolicy.state_rules()` for state reviewer requirements. Give `Timeline` a zoned receipt such as `2026-03-06T09:00:00-08:00[America/Los_Angeles]` so it can apply local calendar deadlines across DST changes. A request's `priority` is read only from structured codes, and free text is never interpreted. HL7 process priority `stat`, or the X12 urgent level of service `U`, gives `expedited`, whatever accompanies it. The X12 emergency level of service `03` gives `emergency`. `normal` or `deferred` gives `standard` only when nothing else in the request speaks in a form the verifier does not read: text, extensions or another coding in `Claim.priority`, or text, a string, an attachment or a reference (such as a physician's report) in `Claim.supportingInfo`. Any of those, a local code, a code without a system, or text alone gives `undetermined`. A treating physician's statement that the standard timeframe could seriously jeopardize the enrollee's health requires an expedited decision (42 CFR 422.570(c)(2)(ii) for Medicare Advantage, with Medicaid and CHIP equivalents) whatever code the requester chose, so the verifier will not certify the standard clock while such content is unread. Process priority codes other than `stat`, `normal` and `deferred` are rejected, as are disagreeing codes in either order, a level of service outside `03`, `E` and `U` or anywhere but the Claim (including on a ClaimResponse or in a notification), and any `coding` that is not a non-empty array of Codings, in requests, responses, subscriptions and every notification including handshakes and heartbeats. A CodeableConcept needs R4 content: a coding, text or a non-empty, well-formed extension; `{"extension": []}`, an `id` alone or an unknown property is rejected. Elements with a required binding need a coded concept; text alone is rejected. For items within the deadline rule, `Timeline.assess_item` raises `PriorityDowngrade` when an expedited request is assessed against the standard clock. It raises `PriorityUndetermined` or `EmergencyLevelOfService` on either clock until the caller passes `determined_priority`: the caller's reading of an undetermined request after reviewing the content the verifier does not read, or of an emergency request once the caller has established that a prior-authorization clock applies. Urgency received outside the request is applied by assessing on the expedited clock, which a standard request always allows. A coded priority cannot be overridden (`PriorityAlreadyCoded`), and a coded `stat` beside the emergency level cannot be determined `standard` (`PriorityDowngrade`). Whether the deadline rule applies is decided before any determination, so drug, payer and effective-date exclusions (and unclassified services) are reported as `outside_scope` whatever `determined_priority` says. `Timeline.assess` takes no request, so it applies none of these request-priority checks. Every review action `reasonCode` in a response must come from X12 external code list 886, its required PAS 2.2.1 binding; another system, a code without one, or text is rejected. X12 licenses that list and PAS does not publish its codes, so pass your licensed copy as `validate_response(..., decision_reason_codes={...})`. Without it a response carrying a reason code raises `TerminologyUnavailable("X12278ReviewDecisionReasonCode")` rather than passing unchecked. A denied item needs such a reason or a line-linked `processNote`. State fields in reviewer contexts and WISeR services must be exact uppercase USPS codes such as `TX`; anything else raises `ValueError` instead of matching no rules.

## Licensed CMS tables

Operators with access to the CMS PTP and MUE release tables can compile them into indexes. Save each table as tab-separated text in one directory. List each file in `manifest.tsv` with its role (`ptp` or `mue`), setting, and, for MUE tables, its effective date and optional exclusive end date. [Manifest and table layouts](crates/medverify-core/src/cms.rs)

```sh
cargo run --release --locked -p medverify-core --bin medverify-index -- cms-tsv --data /path/to/release --out .cache/indexes/licensed
```

Use the Medicare Q4 2026 tables for this command. It labels both indexes `v323r0`, requires MUE effective dates in Q4 2026, and rejects PTP rows effective on or after January 1, 2027. A blank PTP deletion date leaves the interval open.

The compiler discards descriptor and rationale text. Field errors identify the manifest entry, physical table row and field. The compiler publishes both index files together.

Run `medverify-index verify-licensed DIR PTP_ROWS MUE_ROWS` to validate a published pair. The production loader checks both files, and the command requires the licensed catalog, PTP format version 2, MUE format version 1, and the supplied row counts.

## Tests and benchmarks

```sh
cargo test --locked --workspace
python -m uv run --locked pytest -q
```

The Rust benchmarks measure pair lookup, ten-line claim evaluation, and WISeR routing with PAS validation. The scale benchmark runs PTP and MUE evaluation against a synthetic catalog with 4,506,527 PTP rows. [Workloads and measured results](BENCHMARKS.md)

Open a pull request against `main` and wait for the applicable CI jobs to pass. Documentation-only PRs run repository hygiene and secret scanning. Code changes also run the Rust and Python jobs. Changes to FHIR or payer policy, sandbox files, CI, build manifests, or lockfiles also run the sandbox and Inferno suites.

Before tagging a release, use **Actions → CI → Run workflow** on `main`. Confirm that the run tested the commit you intend to tag and that all jobs passed.

## ICD-10-CM

Download and verify the CDC's FY2027 ICD-10-CM code list.

```sh
python -m uv run --locked medverify-ingest icd10 --release FY2027 --out .cache/data/icd10
```

## FHIR sandbox

A Docker Compose sandbox runs HAPI FHIR R4 8.12.0-2 on PostgreSQL 16, the Nirmitee Go payer servers for Da Vinci CRD 2.2.1, DTR 2.2.0 and PAS 2.2.1 behind a test TLS proxy, and the official Inferno server suites from the CRD 0.15.0, DTR 0.18.1 and PAS 0.16.0 test kits. Images are pinned by digest and sources by commit in [sandbox/upstreams.lock](sandbox/upstreams.lock). It needs Docker Engine with Compose and buildx, Bash 4 or later (macOS ships Bash 3.2), and network access while building and validating.

```sh
python -m uv run --locked python scripts/fetch_sandbox.py --locked
docker compose -f sandbox/docker-compose.yaml up --build -d --wait
python -m uv run --locked python scripts/seed_hapi.py --patients 50 --state Washington --seed 42
bash scripts/reset_sandbox.sh --create-template
python -m uv run --locked python scripts/check_reset.py --cycles 2 --expected-patients 50
python -m uv run --locked python scripts/run_inferno.py --crd 2.2.1 --dtr 2.2.0 --pas 2.2.1 --require-pass
```

The seed loads 50 synthetic Washington patients from Synthea with fixed seeds and a fixed simulation date, replaces NPIs with synthetic values, removes licensed code displays, and records a fingerprint of the server content. `bash scripts/reset_sandbox.sh` restores that state: it stops HAPI and the payer servers, drains database connections, recreates the HAPI database from the snapshot, and restarts the payer servers with empty memory. A refreshed snapshot replaces the old one only once it is complete, and a restore interrupted at any step can simply be run again. The seed and reset scripts refuse to write unless their target is the port this compose project publishes. [Latest Inferno results](docs/CONFORMANCE.md)

| Service | Host address | Port variable |
|---|---|---|
| HAPI FHIR R4 | `http://127.0.0.1:18080/fhir` | `MEDVERIFY_HAPI_PORT` |
| CRD, DTR and PAS over test TLS | `https://127.0.0.1:18443` with Host `crd.medverify.test`, `dtr.medverify.test` or `pas.medverify.test` | `MEDVERIFY_TLS_PORT` |
| PAS test-only control routes | `http://127.0.0.1:18085/admin/` | `MEDVERIFY_PAS_PORT` |
| Inferno CRD, DTR and PAS APIs | `http://127.0.0.1:14567`, `14568` and `14569` | `MEDVERIFY_INFERNO_CRD_PORT`, `_DTR_PORT`, `_PAS_PORT` |

Every port binds to 127.0.0.1. The PAS control routes exist only for unattended Inferno runs; the server refuses them through the proxy. To run a second, disposable stack beside an existing one, set `COMPOSE_PROJECT_NAME` and a different value for every port variable before running the commands above; seed and reset records are kept per project under `.cache/sandbox/`.

These suites exercise the sandbox payer servers. MedVerify's own PAS 2.2.1 validator is tested by `cargo test`. `bash scripts/check_sdist.sh` proves the source distribution builds and installs on its own.

## License

[Apache-2.0](LICENSE). See [LICENSING.md](LICENSING.md) for data and third-party rights.
