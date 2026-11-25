//! Signed NCCI evaluation receipts. The operator retains a separate secret for each receipt.

use crate::claim::ClaimLine;
use crate::fhir_pas::{parse_json, REVIEWER_POLICY_TOML};
use crate::index::{MmapMueIndex, MmapPtpIndex};
use crate::mue::{self, MueScope};
use crate::ncci::{self, PtpOutcome};
use crate::wiser::WASHINGTON_POLICY_TOML;
use ed25519_dalek::{Signature, Signer, VerifyingKey};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{fmt, io};
use zeroize::Zeroizing;

pub use ed25519_dalek::SigningKey;

pub const MAX_RECEIPT_BYTES: usize = 4096;
pub const MAX_PUBLIC_KEY_BYTES: usize = 66;

const SCHEMA_VERSION: u8 = 1;
const SIGNATURE_DOMAIN: &[u8] = b"MedVerify receipt signature v1\0";

/// Keep these 32 bytes in the operator's private records. They can open the commitments.
pub struct SecretNonce(Zeroizing<[u8; 32]>);

impl SecretNonce {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiptError {
    InvalidReceipt,
    InvalidPublicKey,
    BadSignature,
    IndexCatalogMismatch,
    Evaluation,
    Random,
    Encoding,
}

impl fmt::Display for ReceiptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidReceipt => "invalid receipt schema",
            Self::InvalidPublicKey => "invalid Ed25519 public key",
            Self::BadSignature => "receipt signature verification failed",
            Self::IndexCatalogMismatch => "index catalogs differ",
            Self::Evaluation => "claim evaluation failed",
            Self::Random => "could not generate receipt nonce",
            Self::Encoding => "could not encode receipt data",
        })
    }
}

impl std::error::Error for ReceiptError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
struct Hex<const N: usize>([u8; N]);

impl<const N: usize> Hex<N> {
    fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        if !bytes
            .iter()
            .all(|&b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err("expected lowercase hexadecimal");
        }
        let mut decoded = [0; N];
        hex::decode_to_slice(bytes, &mut decoded).map_err(|_| "invalid hexadecimal length")?;
        Ok(Self(decoded))
    }
}

impl<const N: usize> TryFrom<String> for Hex<N> {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value.as_bytes())
    }
}

impl<const N: usize> From<Hex<N>> for String {
    fn from(value: Hex<N>) -> Self {
        hex::encode(value.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum FindingCode {
    PtpModifierProhibited,
    PtpSupportedModifierRequired,
    PtpBypassed,
    MueClaimLine,
    MueUnsupportedSplit,
    MuePossibleUnsupportedSplit,
    MueAbsoluteServiceDayLimit,
    MueServiceDayClinicalReview,
}

#[derive(Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    schema_version: u8,
    engine_version: String,
    index_digest: Hex<32>,
    policy_digest: Hex<32>,
    input_commitment: Hex<32>,
    verdict_commitment: Hex<32>,
    finding_codes: Vec<FindingCode>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    payload: Payload,
    signature: Hex<64>,
}

/// A loaded index pair and its receipt digests. Reuse this pair across evaluations.
pub struct ReceiptIndexes<'a> {
    ptp: &'a MmapPtpIndex,
    mue: &'a MmapMueIndex,
    index_digest: Hex<32>,
    policy_digest: Hex<32>,
}

impl<'a> ReceiptIndexes<'a> {
    pub fn new(ptp: &'a MmapPtpIndex, mue: &'a MmapMueIndex) -> Result<Self, ReceiptError> {
        if ptp.catalog_mode() != mue.catalog_mode() {
            return Err(ReceiptError::IndexCatalogMismatch);
        }
        Ok(Self {
            ptp,
            mue,
            index_digest: bundle_digest(
                b"MedVerify index bundle v1\0",
                ptp.file_bytes(),
                mue.file_bytes(),
            ),
            policy_digest: bundle_digest(
                b"MedVerify policy bundle v1\0",
                WASHINGTON_POLICY_TOML.as_bytes(),
                REVIEWER_POLICY_TOML.as_bytes(),
            ),
        })
    }
}

/// A receipt issued by this engine or verified with an independently trusted public key.
#[derive(Debug)]
pub struct Receipt(Envelope);

impl Receipt {
    /// Evaluate NCCI using the supplied loaded indexes and generate a fresh, secret nonce.
    /// The policy digest identifies the engine's bundled WISeR and PAS reviewer tables.
    pub fn issue_ncci(
        indexes: &ReceiptIndexes<'_>,
        lines: &[ClaimLine<'_>],
        signing_key: &SigningKey,
    ) -> Result<(Self, SecretNonce), ReceiptError> {
        let mut nonce = SecretNonce::from_bytes([0; 32]);
        getrandom::fill(nonce.0.as_mut()).map_err(|_| ReceiptError::Random)?;
        let payload = evaluate_ncci(indexes, lines, &nonce)?;
        let signature = Hex(signing_key.sign(&signature_message(&payload)?).to_bytes());
        Ok((Self(Envelope { payload, signature }), nonce))
    }

    pub fn to_json(&self) -> Result<String, ReceiptError> {
        serde_json::to_string(&self.0).map_err(|_| ReceiptError::Encoding)
    }

    /// Verify a receipt with the trusted key's lowercase hex encoding. The key file
    /// can end with a single LF or CRLF. Verification needs no indexes or network.
    pub fn verify_json(document: &[u8], public_key: &[u8]) -> Result<Self, ReceiptError> {
        if public_key.len() > MAX_PUBLIC_KEY_BYTES {
            return Err(ReceiptError::InvalidPublicKey);
        }
        let key = public_key
            .strip_suffix(b"\r\n")
            .or_else(|| public_key.strip_suffix(b"\n"))
            .unwrap_or(public_key);
        let key = Hex::parse(key).map_err(|_| ReceiptError::InvalidPublicKey)?;
        let key = VerifyingKey::from_bytes(&key.0).map_err(|_| ReceiptError::InvalidPublicKey)?;

        if document.len() > MAX_RECEIPT_BYTES {
            return Err(ReceiptError::InvalidReceipt);
        }
        let (envelope, wire) = parse_envelope(document)?;
        let payload = &envelope.payload;
        if payload.schema_version != SCHEMA_VERSION
            || !valid_engine_version(&payload.engine_version)
            || !payload
                .finding_codes
                .windows(2)
                .all(|pair| pair[0] < pair[1])
        {
            return Err(ReceiptError::InvalidReceipt);
        }
        key.verify_strict(
            &signature_message(&wire["payload"])?,
            &Signature::from_bytes(&envelope.signature.0),
        )
        .map_err(|_| ReceiptError::BadSignature)?;
        Ok(Self(envelope))
    }

    /// Locally open an issued or verified receipt by reevaluating the original lines.
    pub fn check_ncci(
        &self,
        indexes: &ReceiptIndexes<'_>,
        lines: &[ClaimLine<'_>],
        nonce: &SecretNonce,
    ) -> Result<bool, ReceiptError> {
        Ok(self.0.payload == evaluate_ncci(indexes, lines, nonce)?)
    }
}

fn parse_envelope(document: &[u8]) -> Result<(Envelope, Value), ReceiptError> {
    let text = std::str::from_utf8(document).map_err(|_| ReceiptError::InvalidReceipt)?;
    let wire = parse_json(text).map_err(|_| ReceiptError::InvalidReceipt)?;
    let envelope = Envelope::deserialize(&wire).map_err(|_| ReceiptError::InvalidReceipt)?;
    // Serde also accepts some alternate shapes, such as sequences for structs.
    // Require the typed envelope to represent the very same JSON value.
    if serde_json::to_value(&envelope).map_err(|_| ReceiptError::Encoding)? != wire {
        return Err(ReceiptError::InvalidReceipt);
    }
    Ok((envelope, wire))
}

fn valid_engine_version(version: &str) -> bool {
    let mut parts = version.split('.');
    (0..3).all(|_| {
        parts.next().is_some_and(|part| {
            (part == "0" || !part.starts_with('0'))
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && part.parse::<u32>().is_ok()
        })
    }) && parts.next().is_none()
}

fn signature_message(payload: &impl Serialize) -> Result<Vec<u8>, ReceiptError> {
    let mut message = SIGNATURE_DOMAIN.to_vec();
    serde_json_canonicalizer::to_writer(payload, &mut message)
        .map_err(|_| ReceiptError::Encoding)?;
    Ok(message)
}

fn bundle_digest(domain: &[u8], first: &[u8], second: &[u8]) -> Hex<32> {
    let hash = Sha256::new()
        .chain_update(domain)
        .chain_update((first.len() as u64).to_be_bytes())
        .chain_update(first)
        .chain_update((second.len() as u64).to_be_bytes())
        .chain_update(second);
    Hex(hash.finalize().into())
}

struct CommitmentWriter(Hmac<Sha256>);

impl io::Write for CommitmentWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn commitment(
    nonce: &SecretNonce,
    domain: &[u8],
    value: &(impl Serialize + ?Sized),
) -> Result<Hex<32>, ReceiptError> {
    let mut mac = CommitmentWriter(
        Hmac::new_from_slice(nonce.as_bytes()).expect("HMAC accepts keys of any length"),
    );
    mac.0.update(domain);
    // Stream the private data into the MAC, preserving full-width integers.
    serde_json::to_writer(&mut mac, value).map_err(|_| ReceiptError::Encoding)?;
    Ok(Hex(mac.0.finalize().into_bytes().into()))
}

fn evaluate_ncci(
    indexes: &ReceiptIndexes<'_>,
    lines: &[ClaimLine<'_>],
    nonce: &SecretNonce,
) -> Result<Payload, ReceiptError> {
    let pairs = ncci::evaluate(indexes.ptp, lines).map_err(|_| ReceiptError::Evaluation)?;
    let limits = mue::evaluate(indexes.mue, lines).map_err(|_| ReceiptError::Evaluation)?;
    let mut finding_codes: Vec<_> = pairs
        .iter()
        .map(|pair| match pair.outcome {
            PtpOutcome::ModifierProhibited => FindingCode::PtpModifierProhibited,
            PtpOutcome::SupportedModifierRequired => FindingCode::PtpSupportedModifierRequired,
            PtpOutcome::Bypassed => FindingCode::PtpBypassed,
        })
        .chain(limits.iter().map(|limit| match limit.scope {
            MueScope::ClaimLine { .. } => FindingCode::MueClaimLine,
            MueScope::UnsupportedSplit { .. } => FindingCode::MueUnsupportedSplit,
            MueScope::PossibleUnsupportedSplit { .. } => FindingCode::MuePossibleUnsupportedSplit,
            MueScope::AbsoluteServiceDayLimit => FindingCode::MueAbsoluteServiceDayLimit,
            MueScope::ServiceDayClinicalReview => FindingCode::MueServiceDayClinicalReview,
        }))
        .collect();
    finding_codes.sort_unstable();
    finding_codes.dedup();

    Ok(Payload {
        schema_version: SCHEMA_VERSION,
        engine_version: env!("CARGO_PKG_VERSION").to_owned(),
        index_digest: indexes.index_digest,
        policy_digest: indexes.policy_digest,
        input_commitment: commitment(nonce, b"MedVerify NCCI input v1\0", lines)?,
        verdict_commitment: commitment(nonce, b"MedVerify NCCI verdict v1\0", &(pairs, limits))?,
        finding_codes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claim::{ProcedureCode, ServiceDate, ServiceDay, ServiceKind, Setting};
    use crate::encoding::{MueRecord, NcciPtpRecord, NO_DELETION_DATE};
    use crate::index::{write_mue, write_ptp, CatalogMode};
    use std::fs::File;
    use std::num::NonZeroU64;
    use time::macros::date;

    #[test]
    fn receipt_commits_to_loaded_bytes_and_keeps_private_data_out() {
        let directory = tempfile::tempdir().unwrap();
        let ptp_path = directory.path().join("ptp.bin");
        let mue_path = directory.path().join("mue.bin");
        let code = ProcedureCode::new("Y9011").unwrap();
        let date = ServiceDate::new(date!(2026 - 10 - 01)).unwrap();
        let ptp_row = NcciPtpRecord {
            col1_b36: code.0,
            col2_b36: ProcedureCode::new("Y9012").unwrap().0,
            eff_days: date.0,
            del_days: NO_DELETION_DATE,
            setting_flags: Setting::Practitioner as u8,
            ccmi: 0,
            rationale_id: 0,
            flags: 0,
        };
        let mue_row = MueRecord {
            code_b36: code.0,
            max_units: 3,
            eff_days: date.0,
            del_days: NO_DELETION_DATE,
            setting_flags: Setting::Practitioner as u8,
            mai: 1,
            rationale_id: 0,
            flags: 0,
            reserved: 0,
        };
        write_ptp(
            File::create(&ptp_path).unwrap(),
            CatalogMode::Synthetic,
            vec![ptp_row],
        )
        .unwrap();
        write_mue(
            File::create(&mue_path).unwrap(),
            CatalogMode::Synthetic,
            vec![mue_row],
        )
        .unwrap();
        let ptp = MmapPtpIndex::open(&ptp_path).unwrap();
        let mue = MmapMueIndex::open(&mue_path).unwrap();
        let indexes = ReceiptIndexes::new(&ptp, &mue).unwrap();
        let line = ClaimLine {
            id: 42,
            service_day: ServiceDay {
                beneficiary: "CANARY_PATIENT Clinical narrative",
                billing_provider: "CANARY_PROVIDER",
                date,
                setting: Setting::Practitioner,
            },
            code,
            service_kind: ServiceKind::Other,
            units: NonZeroU64::new(u64::MAX).unwrap(),
            modifiers: &[],
            modifier_evidence: &[],
            abn_liability_shift_attempted: false,
            claim_id: Some("CANARY_CLAIM"),
        };
        let lines = [line];
        let key = SigningKey::from_bytes(&[42; 32]);
        let public = hex::encode(key.verifying_key().to_bytes());
        let (receipt, nonce) = Receipt::issue_ncci(&indexes, &lines, &key).unwrap();
        let (second, second_nonce) = std::thread::scope(|scope| {
            let issuance = scope.spawn(|| Receipt::issue_ncci(&indexes, &lines, &key).unwrap());
            assert!(receipt.check_ncci(&indexes, &lines, &nonce).unwrap());
            issuance.join().unwrap()
        });
        assert_eq!(
            receipt.0.payload.index_digest,
            second.0.payload.index_digest
        );
        let document = receipt.to_json().unwrap();
        let opened = Receipt::verify_json(document.as_bytes(), public.as_bytes()).unwrap();
        assert!(opened.check_ncci(&indexes, &lines, &nonce).unwrap());
        assert!(!opened.check_ncci(&indexes, &lines, &second_nonce).unwrap());
        assert_ne!(
            receipt.0.payload.input_commitment,
            second.0.payload.input_commitment
        );
        assert_ne!(
            receipt.0.payload.verdict_commitment,
            second.0.payload.verdict_commitment
        );
        let changed = [ClaimLine {
            units: NonZeroU64::new(u64::MAX - 1).unwrap(),
            ..line
        }];
        assert!(!opened.check_ncci(&indexes, &changed, &nonce).unwrap());
        let changed = evaluate_ncci(&indexes, &changed, &nonce).unwrap();
        assert_ne!(receipt.0.payload.input_commitment, changed.input_commitment);
        assert_ne!(
            receipt.0.payload.verdict_commitment,
            changed.verdict_commitment
        );
        assert_eq!(
            receipt.0.payload.finding_codes,
            vec![FindingCode::MueClaimLine]
        );
        for canary in [
            "CANARY_",
            "Clinical narrative",
            "2026-10-01",
            "Y9011",
            "18446744073709551615",
            "total_units",
            "line_id",
            &hex::encode(nonce.as_bytes()),
        ] {
            assert!(!document.contains(canary));
        }

        // A rewritten file must not change the identity of an existing anonymous mapping.
        crate::index::write_ptp_v1(
            File::create(&ptp_path).unwrap(),
            CatalogMode::Synthetic,
            vec![ptp_row],
        )
        .unwrap();
        let reopened = MmapPtpIndex::open(&ptp_path).unwrap();
        assert_eq!(ptp.records(), reopened.records());
        let replaced_ptp = ReceiptIndexes::new(&reopened, &mue).unwrap();
        assert!(!opened.check_ncci(&replaced_ptp, &lines, &nonce).unwrap());

        // Modify an unrelated MUE row so the evaluated findings remain the same.
        let unrelated = MueRecord {
            code_b36: ptp_row.col2_b36,
            ..mue_row
        };
        write_mue(
            File::create(&mue_path).unwrap(),
            CatalogMode::Synthetic,
            vec![mue_row, unrelated],
        )
        .unwrap();
        let reopened = MmapMueIndex::open(&mue_path).unwrap();
        let replaced_mue = ReceiptIndexes::new(&ptp, &reopened).unwrap();
        assert_ne!(indexes.index_digest, replaced_mue.index_digest);
        assert!(!opened.check_ncci(&replaced_mue, &lines, &nonce).unwrap());
        assert!(opened.check_ncci(&indexes, &lines, &nonce).unwrap());
        let rebuilt_from_loaded = ReceiptIndexes::new(&ptp, &mue).unwrap();
        assert!(opened
            .check_ncci(&rebuilt_from_loaded, &lines, &nonce)
            .unwrap());
    }

    #[test]
    fn receipt_schema_rejects_alternate_json_shapes() {
        use serde_json::json;

        let empty_digest = hex::encode([0; 32]);
        let mut envelope = json!({
            "signature": hex::encode([0; 64]),
            "payload": {
                "schema_version": 1,
                "engine_version": "0.2.0",
                "index_digest": empty_digest,
                "policy_digest": empty_digest,
                "input_commitment": empty_digest,
                "verdict_commitment": empty_digest,
                "finding_codes": ["mue_claim_line"],
            },
        });
        assert!(parse_envelope(envelope.to_string().as_bytes()).is_ok());
        let signature = envelope["signature"].clone();
        let payload = envelope["payload"].clone();
        let envelope_array = json!([payload, signature]);
        assert!(parse_envelope(envelope_array.to_string().as_bytes()).is_err());
        envelope["payload"] = json!([
            1,
            "0.2.0",
            empty_digest,
            empty_digest,
            empty_digest,
            empty_digest,
            ["mue_claim_line"]
        ]);
        assert!(parse_envelope(envelope.to_string().as_bytes()).is_err());
        envelope["payload"] = payload;
        envelope["payload"]["finding_codes"] = json!([{"mue_claim_line": null}]);
        assert!(parse_envelope(envelope.to_string().as_bytes()).is_err());
    }

    #[test]
    fn signed_message_is_domain_separated_jcs() {
        let payload = Payload {
            schema_version: 1,
            engine_version: "0.2.0".to_owned(),
            index_digest: Hex([1; 32]),
            policy_digest: Hex([2; 32]),
            input_commitment: Hex([3; 32]),
            verdict_commitment: Hex([4; 32]),
            finding_codes: vec![FindingCode::PtpBypassed],
        };
        let expected = format!(
            concat!(
                "MedVerify receipt signature v1\0",
                r#"{{"engine_version":"0.2.0","finding_codes":["ptp_bypassed"],"index_digest":"{}","input_commitment":"{}","policy_digest":"{}","schema_version":1,"verdict_commitment":"{}"}}"#,
            ),
            "01".repeat(32),
            "03".repeat(32),
            "02".repeat(32),
            "04".repeat(32),
        );
        assert_eq!(signature_message(&payload).unwrap(), expected.as_bytes());
    }
}
