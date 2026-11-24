//! PAS 2.2.1 checks for R4 JSON submissions, responses and subscription events.

mod policy;
pub use policy::*;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

const PAS: &str = "http://hl7.org/fhir/us/davinci-pas/StructureDefinition/";
const BACKPORT: &str = "http://hl7.org/fhir/uv/subscriptions-backport/StructureDefinition/";
const ACTION_SYSTEM: &str = "https://codesystem.x12.org/005010/306";
const TOPIC: &str = "http://hl7.org/fhir/us/davinci-pas/SubscriptionTopic/PASSubscriptionTopic";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Bundle,
    Profile(&'static str),
    Missing(&'static str),
    DuplicateResource,
    Reference,
    ReferenceType,
    ClaimCorrelation,
    ItemCorrelation,
    ReviewAction,
    ApprovalNumber,
    DenialReason,
    PendingInformation,
    Modification,
    Subscription,
    Notification,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Error {}

fn required_text(value: &Value) -> Option<&str> {
    value.as_str().filter(|value| !value.trim().is_empty())
}

fn list(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

fn required_list<'a>(value: &'a Value, field: &'static str) -> Result<&'a [Value], Error> {
    match_entries(value).ok_or(Error::Missing(field))
}

fn match_entries(value: &Value) -> Option<&[Value]> {
    value
        .as_array()
        .filter(|array| !array.is_empty())
        .map(Vec::as_slice)
}

fn optional_list<'a>(parent: &'a Value, field: &'static str) -> Result<&'a [Value], Error> {
    parent
        .get(field)
        .map_or(Ok(&[]), |value| required_list(value, field))
}

fn positive(value: &Value) -> Option<u32> {
    u32::try_from(value.as_u64()?)
        .ok()
        .filter(|value| (1..=i32::MAX as u32).contains(value))
}

fn instant(value: &Value) -> bool {
    required_text(value).is_some_and(|value| OffsetDateTime::parse(value, &Rfc3339).is_ok())
}

fn identifier(value: &Value) -> Option<(&str, &str)> {
    Some((
        required_text(&value["system"])?,
        required_text(&value["value"])?,
    ))
}

fn codings(value: &Value) -> impl Iterator<Item = (&str, &str)> {
    list(&value["coding"]).iter().filter_map(|coding| {
        Some((
            required_text(&coding["system"])?,
            required_text(&coding["code"])?,
        ))
    })
}

fn concept(value: &Value) -> bool {
    codings(value).next().is_some() || required_text(&value["text"]).is_some()
}

fn extension<'a>(value: &'a Value, base: &str, name: &str) -> Result<Option<&'a Value>, Error> {
    let mut entries = optional_list(value, "extension")?
        .iter()
        .filter(|entry| entry["url"].as_str().and_then(|url| url.strip_prefix(base)) == Some(name));
    let entry = entries.next();
    if entries.next().is_some() {
        return Err(Error::Profile("extension cardinality"));
    }
    Ok(entry)
}

fn profile(value: &Value, kind: &'static str, name: &'static str) -> Result<(), Error> {
    profile_among(value, kind, name, &[name])
}

/// Like `profile`, for roles whose published target profiles are interchangeable for the resource,
/// reporting `name` when the resource declares only PAS profiles outside `accepted`.
fn profile_among(
    value: &Value,
    kind: &'static str,
    name: &'static str,
    accepted: &[&str],
) -> Result<(), Error> {
    if value["resourceType"] != kind || value.get("meta").is_some_and(|meta| !meta.is_object()) {
        return Err(Error::Profile(name));
    }
    if let Some(profiles) = value["meta"].get("profile") {
        let mut any_pas = false;
        let mut matched = false;
        for url in required_list(profiles, "meta.profile")? {
            let url = required_text(url).ok_or(Error::Profile(name))?;
            if let Some(local) = url.strip_prefix(PAS) {
                any_pas = true;
                if accepted.contains(&local.strip_suffix("|2.2.1").unwrap_or(local)) {
                    matched = true;
                }
            }
        }
        if any_pas && !matched {
            return Err(Error::Profile(name));
        }
    }
    Ok(())
}

fn rest_url(value: &str) -> bool {
    value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
        .is_some_and(|tail| {
            let authority = tail.split(['/', '?', '#']).next().unwrap_or_default();
            !authority.is_empty()
                && !authority.contains('@')
                && !value.contains(['\\', '#'])
                && !value.chars().any(char::is_whitespace)
        })
}

fn full_url(value: &str) -> bool {
    (rest_url(value) && !value.contains("/_history/") && !value.contains('?'))
        || value.strip_prefix("urn:uuid:").is_some_and(|id| {
            id.len() == 36
                && id.bytes().enumerate().all(|(index, byte)| {
                    if matches!(index, 8 | 13 | 18 | 23) {
                        byte == b'-'
                    } else {
                        byte.is_ascii_hexdigit()
                    }
                })
        })
        || value.strip_prefix("urn:oid:").is_some_and(|oid| {
            oid.contains('.')
                && oid.split('.').all(|component| {
                    !component.is_empty() && component.bytes().all(|b| b.is_ascii_digit())
                })
        })
}

#[derive(Clone, Copy)]
struct Entry<'a> {
    url: &'a str,
    resource: &'a Value,
}

struct Bundle<'a> {
    entries: Vec<Entry<'a>>,
    by_url: BTreeMap<&'a str, Entry<'a>>,
}

impl<'a> Bundle<'a> {
    fn collection(value: &'a Value, name: &'static str) -> Result<Self, Error> {
        profile(value, "Bundle", name)?;
        // FHIR R4 bdl-1: total is only allowed for searchset and history Bundles.
        if value["type"] != "collection"
            || value.get("total").is_some()
            || identifier(&value["identifier"]).is_none()
            || !instant(&value["timestamp"])
        {
            return Err(Error::Bundle);
        }
        let mut entries = Vec::new();
        let mut by_url = BTreeMap::new();
        for entry in required_list(&value["entry"], "Bundle.entry")? {
            if entry.get("request").is_some()
                || entry.get("response").is_some()
                || entry.get("search").is_some()
            {
                return Err(Error::Bundle);
            }
            let url = required_text(&entry["fullUrl"])
                .filter(|url| full_url(url))
                .ok_or(Error::Bundle)?;
            let resource = &entry["resource"];
            let kind = required_text(&resource["resourceType"]).ok_or(Error::Bundle)?;
            if let Some(id) = resource.get("id") {
                let id = required_text(id)
                    .filter(|id| fhir_id(id))
                    .ok_or(Error::Bundle)?;
                if let Some((_, url_kind, url_id)) = rest_identity(url) {
                    if id != url_id || kind != url_kind {
                        return Err(Error::Bundle);
                    }
                }
            }
            let entry = Entry { url, resource };
            if by_url.insert(url, entry).is_some() {
                return Err(Error::DuplicateResource);
            }
            entries.push(entry);
        }
        Ok(Self { entries, by_url })
    }

    fn resolve(&self, from: Entry<'a>, reference: &str) -> Option<Entry<'a>> {
        if let Some(id) = reference.strip_prefix('#') {
            let resource = list(&from.resource["contained"])
                .iter()
                .find(|value| value["id"] == id)?;
            return Some(Entry {
                url: from.url,
                resource,
            });
        }
        let (reference, version) = reference
            .rsplit_once("/_history/")
            .map_or((reference, None), |(reference, version)| {
                (reference, Some(version))
            });
        let entry = if let Some(entry) = self.by_url.get(reference) {
            *entry
        } else {
            let (kind, id) = reference.split_once('/')?;
            if !fhir_id(id) || !kind.bytes().all(|byte| byte.is_ascii_alphabetic()) {
                return None;
            }
            let (base, _, _) = rest_identity(from.url)?;
            *self.by_url.get(format!("{base}/{kind}/{id}").as_str())?
        };
        version
            .is_none_or(|version| {
                fhir_id(version) && entry.resource["meta"]["versionId"] == version
            })
            .then_some(entry)
    }

    fn target(&self, from: Entry<'a>, field: &'a Value, kind: &str) -> Result<Entry<'a>, Error> {
        let reference = required_text(&field["reference"]).ok_or(Error::Reference)?;
        let target = self.resolve(from, reference).ok_or(Error::Reference)?;
        if target.resource["resourceType"] != kind {
            return Err(Error::ReferenceType);
        }
        Ok(target)
    }

    fn references(
        &self,
        source: Entry<'a>,
        external_reference: Option<&Value>,
        allow_cancellation: bool,
    ) -> Result<(), Error> {
        self.walk(
            source,
            source.resource,
            external_reference,
            allow_cancellation,
        )
    }

    fn walk(
        &self,
        source: Entry<'a>,
        value: &Value,
        external_reference: Option<&Value>,
        allow_cancellation: bool,
    ) -> Result<(), Error> {
        if external_reference.is_some_and(|reference| std::ptr::eq(value, reference)) {
            return Ok(());
        }
        match value {
            Value::Array(values) => {
                for value in values {
                    self.walk(source, value, external_reference, allow_cancellation)?;
                }
            }
            Value::Object(elements) => {
                if let Some(reference) = elements.get("reference") {
                    let reference = required_text(reference).ok_or(Error::Reference)?;
                    let target = self.resolve(source, reference).ok_or(Error::Reference)?;
                    if let Some(kind) = required_text(&value["type"]) {
                        if target.resource["resourceType"]
                            != kind.rsplit('/').next().unwrap_or(kind)
                        {
                            return Err(Error::ReferenceType);
                        }
                    }
                }
                for (key, nested) in elements {
                    if key == "text" {
                        continue;
                    }
                    // Unknown modifier extensions cannot be treated as ordinary supporting data.
                    if key == "modifierExtension"
                        && !match_entries(nested).is_some_and(|extensions| {
                            allow_cancellation
                                && extensions.len() == 1
                                && extensions[0]["url"]
                                    == format!("{PAS}modifierextension-infoCancelled")
                                && extensions[0]["valueBoolean"].is_boolean()
                        })
                    {
                        return Err(Error::Profile("unsupported modifierExtension"));
                    }
                    self.walk(source, nested, external_reference, allow_cancellation)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

fn fhir_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
}

fn rest_identity(url: &str) -> Option<(&str, &str, &str)> {
    if !rest_url(url) {
        return None;
    }
    let (prefix, id) = url.rsplit_once('/')?;
    let (base, kind) = prefix.rsplit_once('/')?;
    (rest_url(base)
        && fhir_id(id)
        && !kind.is_empty()
        && kind.bytes().all(|byte| byte.is_ascii_alphabetic()))
    .then_some((base, kind, id))
}

/// Both PAS 2.2.1 Patient profiles derive from US Core Patient 7.0.0 and differ only by optional
/// elements (profile-subscriber adds extension-militaryStatus 0..1; profile-beneficiary adds the
/// multipleBirthInteger slice 0..1). Claim.patient and Coverage.beneficiary point at
/// profile-beneficiary and Coverage.subscriber at profile-subscriber, but the official examples
/// reuse one profile-subscriber Patient for the self-insured case. A targetProfile is satisfied by
/// content conformance, so a Patient that declares either profile fills either role. The content
/// checks below cover required elements both profiles share: identifier 1..* with system and
/// value, name 1..*, gender 1..1. ClaimResponse.patient has the same profile-beneficiary target.
const PATIENT_PROFILES: [&str; 2] = ["profile-beneficiary", "profile-subscriber"];

fn patient(value: &Value, role: &'static str) -> Result<(), Error> {
    profile_among(value, "Patient", role, &PATIENT_PROFILES)?;
    if !list(&value["identifier"])
        .iter()
        .any(|value| identifier(value).is_some())
        || !list(&value["name"]).iter().any(Value::is_object)
        || !matches!(
            value["gender"].as_str(),
            Some("male" | "female" | "other" | "unknown")
        )
    {
        return Err(Error::Profile(role));
    }
    Ok(())
}

fn organization(value: &Value, name: &'static str) -> Result<(), Error> {
    profile(value, "Organization", name)?;
    if value["active"].as_bool().is_none() || required_text(&value["name"]).is_none() {
        return Err(Error::Profile(name));
    }
    Ok(())
}

fn claim_fields(value: &Value) -> bool {
    value["status"] == "active"
        && value["use"] == "preauthorization"
        && concept(&value["type"])
        && instant(&value["created"])
}

fn valid_local_sequences(value: &Value, collection: &str, references: &str, item: &Value) -> bool {
    let Some(references) = item.get(references) else {
        return true;
    };
    let Some(references) = match_entries(references) else {
        return false;
    };
    let mut seen = BTreeSet::new();
    references.iter().all(|reference| {
        positive(reference).is_some_and(|number| {
            seen.insert(number)
                && list(&value[collection])
                    .iter()
                    .any(|entry| positive(&entry["sequence"]) == Some(number))
        })
    })
}

#[derive(Clone, Copy, Debug)]
pub struct RequestItem<'a> {
    pub sequence: u32,
    pub product_or_service: &'a Value,
}

impl RequestItem<'_> {
    /// HCPCS and other mixed code systems require the caller's dated product classification.
    pub fn service_kind(
        &self,
        classify: impl Fn(&str, &str) -> Option<ServiceKind>,
    ) -> Option<ServiceKind> {
        let mut medical = false;
        for (system, code) in codings(self.product_or_service) {
            let kind = match system {
                "http://hl7.org/fhir/sid/ndc" | "http://www.nlm.nih.gov/research/umls/rxnorm" => {
                    Some(ServiceKind::Drug)
                }
                _ => classify(system, code),
            };
            match kind {
                Some(ServiceKind::Drug) => return Some(ServiceKind::Drug),
                Some(ServiceKind::Medical) => medical = true,
                None => {}
            }
        }
        medical.then_some(ServiceKind::Medical)
    }
}

pub struct Request<'a> {
    claim_url: Cow<'a, str>,
    claim: Cow<'a, Value>,
    patient: Party<'a>,
    insurer: Party<'a>,
    items: BTreeMap<u32, Cow<'a, Value>>,
    priority: Priority,
}

struct Party<'a> {
    url: Cow<'a, str>,
    id: Cow<'a, Value>,
}

impl<'a> Party<'a> {
    fn from_entry(entry: Entry<'a>) -> Self {
        Self {
            url: Cow::Borrowed(entry.url),
            id: Cow::Borrowed(&entry.resource["id"]),
        }
    }

    fn into_owned(self) -> Party<'static> {
        Party {
            url: Cow::Owned(self.url.into_owned()),
            id: Cow::Owned(self.id.into_owned()),
        }
    }
}

impl<'a> Request<'a> {
    pub fn items(&self) -> impl Iterator<Item = RequestItem<'_>> + '_ {
        self.items
            .iter()
            .map(|(&sequence, product_or_service)| RequestItem {
                sequence,
                product_or_service,
            })
    }

    pub fn priority(&self) -> Priority {
        self.priority
    }

    /// Retain the validated request data needed for response correlation and item classification.
    pub fn into_owned(self) -> Request<'static> {
        Request {
            claim_url: Cow::Owned(self.claim_url.into_owned()),
            claim: Cow::Owned(self.claim.into_owned()),
            patient: self.patient.into_owned(),
            insurer: self.insurer.into_owned(),
            items: self
                .items
                .into_iter()
                .map(|(sequence, item)| (sequence, Cow::Owned(item.into_owned())))
                .collect(),
            priority: self.priority,
        }
    }
}

/// Validate a PAS $submit request. R4 JSON can be parsed with serde_json before validation.
pub fn validate_request(value: &Value) -> Result<Request<'_>, Error> {
    let bundle = Bundle::collection(value, "profile-pas-request-bundle")?;
    let claim = bundle.entries[0];
    let resource = claim.resource;
    if resource["resourceType"] != "Claim" {
        return Err(Error::Profile("first request entry must be Claim"));
    }
    let previous = previous_claim(&bundle, claim)?;
    request_claim_fields(resource, previous.is_some())?;
    let patient_entry = bundle.target(claim, &resource["patient"], "Patient")?;
    patient(patient_entry.resource, "profile-beneficiary")?;
    let insurer = bundle.target(claim, &resource["insurer"], "Organization")?;
    organization(insurer.resource, "profile-insurer")?;
    let provider = bundle
        .resolve(
            claim,
            required_text(&resource["provider"]["reference"]).ok_or(Error::Reference)?,
        )
        .ok_or(Error::Reference)?;
    match provider.resource["resourceType"].as_str() {
        Some("Organization") => organization(provider.resource, "profile-requestor")?,
        Some("PractitionerRole") => {
            profile(
                provider.resource,
                "PractitionerRole",
                "profile-practitionerrole",
            )?;
            let practitioner =
                bundle.target(provider, &provider.resource["practitioner"], "Practitioner")?;
            profile(
                practitioner.resource,
                "Practitioner",
                "profile-practitioner",
            )?;
        }
        _ => return Err(Error::ReferenceType),
    }
    let mut focus_count = 0;
    let mut insurance_sequences = BTreeSet::new();
    for insurance in required_list(&resource["insurance"], "Claim.insurance")? {
        let sequence = positive(&insurance["sequence"]).ok_or(Error::ItemCorrelation)?;
        if !insurance_sequences.insert(sequence) {
            return Err(Error::ItemCorrelation);
        }
        let focal = insurance["focal"]
            .as_bool()
            .ok_or(Error::Profile("Claim.insurance.focal"))?;
        focus_count += usize::from(focal);
        let coverage = bundle.target(claim, &insurance["coverage"], "Coverage")?;
        profile(coverage.resource, "Coverage", "profile-coverage")?;
        let beneficiary = bundle.target(coverage, &coverage.resource["beneficiary"], "Patient")?;
        // profile-coverage invariant self-beneficiary: without the X12 self code 18 (the published
        // expression compares code only), the subscriber SHALL be present. Coverage.subscriber
        // points at profile-subscriber.
        match coverage.resource.get("subscriber") {
            Some(subscriber) => patient(
                bundle.target(coverage, subscriber, "Patient")?.resource,
                "profile-subscriber",
            )?,
            None => {
                if list(&coverage.resource["relationship"]["coding"])
                    .iter()
                    .all(|coding| coding["code"] != "18")
                {
                    return Err(Error::Missing("Coverage.subscriber"));
                }
            }
        }
        let payors = required_list(&coverage.resource["payor"], "Coverage.payor")?;
        let payor = bundle.target(coverage, &payors[0], "Organization")?;
        if payors.len() != 1
            || coverage.resource["status"] != "active"
            || !concept(&coverage.resource["relationship"])
            // The memberid slice is discriminated by the MB type pattern alone; its system and
            // value are 0..1, and the official examples carry the id in Coverage.subscriberId.
            || list(&coverage.resource["identifier"])
                .iter()
                .filter(|value| {
                    codings(&value["type"]).any(|coding| {
                        coding == ("http://terminology.hl7.org/CodeSystem/v2-0203", "MB")
                    })
                })
                .count()
                != 1
            || !std::ptr::eq(beneficiary.resource, patient_entry.resource)
            || (focal && !std::ptr::eq(payor.resource, insurer.resource))
        {
            return Err(Error::Profile("profile-coverage"));
        }
    }
    if focus_count != 1 {
        return Err(Error::Profile("Claim.insurance.focal"));
    }
    let mut priority = match codings(&resource["priority"]).find_map(|(system, code)| {
        (system == "http://terminology.hl7.org/CodeSystem/processpriority").then_some(code)
    }) {
        Some("stat" | "urgent" | "asap") => Priority::Expedited,
        Some("normal") => Priority::Standard,
        _ => return Err(Error::Profile("Claim.priority")),
    };
    if extension(resource, PAS, "extension-levelOfServiceCode")?.is_some_and(|level| {
        codings(&level["valueCodeableConcept"])
            .any(|coding| coding == ("https://codesystem.x12.org/005010/1338", "U"))
    }) {
        priority = Priority::Expedited;
    }
    for collection in ["careTeam", "diagnosis", "procedure", "supportingInfo"] {
        if let Some(array) = resource.get(collection) {
            let mut sequences = BTreeSet::new();
            if !required_list(array, "Claim sequence collection")?
                .iter()
                .all(|entry| {
                    positive(&entry["sequence"]).is_some_and(|number| sequences.insert(number))
                })
            {
                return Err(Error::ItemCorrelation);
            }
        }
    }
    claim_codes(resource)?;
    let mut items = BTreeMap::new();
    let cancelling = previous.is_some() && certification_is(resource, "3")?;
    let request_items = if cancelling && resource.get("item").is_none() {
        &[]
    } else {
        required_list(&resource["item"], "Claim.item")?
    };
    for item in request_items {
        let sequence = positive(&item["sequence"]).ok_or(Error::ItemCorrelation)?;
        let product_or_service = &item["productOrService"];
        let locations = [
            "locationCodeableConcept",
            "locationAddress",
            "locationReference",
        ];
        if !concept(product_or_service)
            || !concept(&item["category"])
            || locations
                .iter()
                .filter(|field| item[**field].is_object())
                .count()
                != 1
            || locations
                .iter()
                .any(|field| item.get(field).is_some_and(|value| !value.is_object()))
        {
            return Err(Error::Profile("Claim.item"));
        }
        for name in [
            "extension-serviceItemRequestType",
            "extension-certificationType",
        ] {
            let system = if name == "extension-certificationType" {
                "https://codesystem.x12.org/005010/1322"
            } else {
                "https://codesystem.x12.org/005010/1525"
            };
            if !extension(item, PAS, name)?.is_some_and(|entry| {
                codings(&entry["valueCodeableConcept"])
                    .any(|(entry_system, _)| entry_system == system)
            }) {
                return Err(Error::Profile("Claim.item extension"));
            }
        }
        for (collection, references) in [
            ("careTeam", "careTeamSequence"),
            ("diagnosis", "diagnosisSequence"),
            ("procedure", "procedureSequence"),
            ("supportingInfo", "informationSequence"),
        ] {
            if !valid_local_sequences(resource, collection, references, item) {
                return Err(Error::ItemCorrelation);
            }
        }
        if items
            .insert(sequence, Cow::Borrowed(product_or_service))
            .is_some()
        {
            return Err(Error::ItemCorrelation);
        }
    }
    if let Some(previous) = previous {
        if !cancelling
            && list(&previous.resource["item"]).iter().any(|item| {
                !positive(&item["sequence"]).is_some_and(|sequence| items.contains_key(&sequence))
            })
        {
            return Err(Error::ItemCorrelation);
        }
    }
    for entry in &bundle.entries {
        let superseded = previous
            .filter(|previous| previous.url == entry.url)
            .and_then(|previous| list(&previous.resource["related"]).first())
            .and_then(|related| related.get("claim"));
        bundle.references(
            *entry,
            superseded,
            previous.is_some() && entry.resource["resourceType"] == "Claim",
        )?;
    }
    Ok(Request {
        claim_url: Cow::Borrowed(claim.url),
        claim: Cow::Borrowed(claim.resource),
        patient: Party::from_entry(patient_entry),
        insurer: Party::from_entry(insurer),
        items,
        priority,
    })
}

/// profile-claim cardinalities: Claim.diagnosis.diagnosis[x] 1..1 restricted to CodeableConcept
/// (Claim.diagnosis.diagnosisReference is not an allowed type), and Claim.procedure.procedure[x]
/// 1..1 as CodeableConcept or Reference.
fn claim_codes(claim: &Value) -> Result<(), Error> {
    for entry in optional_list(claim, "diagnosis")? {
        if entry.get("diagnosisReference").is_some() {
            return Err(Error::Profile("Claim.diagnosis.diagnosis[x]"));
        }
        if !concept(&entry["diagnosisCodeableConcept"]) {
            return Err(Error::Missing("Claim.diagnosis.diagnosis[x]"));
        }
    }
    for entry in optional_list(claim, "procedure")? {
        let coded = entry.get("procedureCodeableConcept");
        let referenced = entry.get("procedureReference");
        if coded.is_some() && referenced.is_some() {
            return Err(Error::Profile("Claim.procedure.procedure[x]"));
        }
        let present = match (coded, referenced) {
            (Some(coded), None) => concept(coded),
            (None, Some(reference)) => {
                required_text(&reference["reference"]).is_some()
                    || identifier(&reference["identifier"]).is_some()
            }
            _ => false,
        };
        if !present {
            return Err(Error::Missing("Claim.procedure.procedure[x]"));
        }
    }
    Ok(())
}

fn request_claim_fields(value: &Value, update: bool) -> Result<(), Error> {
    let name = if update {
        "profile-claim-update"
    } else {
        "profile-claim"
    };
    profile(value, "Claim", name)?;
    let identifiers = required_list(&value["identifier"], "Claim.identifier")?;
    if !claim_fields(value)
        || identifiers.len() > 2
        || identifiers.iter().any(|value| identifier(value).is_none())
    {
        return Err(Error::Profile(name));
    }
    Ok(())
}

fn previous_claim<'a>(bundle: &Bundle<'a>, claim: Entry<'a>) -> Result<Option<Entry<'a>>, Error> {
    let mut other_claims = bundle
        .entries
        .iter()
        .skip(1)
        .filter(|entry| entry.resource["resourceType"] == "Claim");
    let previous = other_claims.next().copied();
    if other_claims.next().is_some() {
        return Err(Error::ClaimCorrelation);
    }
    let Some(previous) = previous else {
        return if claim.resource.get("related").is_none() {
            Ok(None)
        } else {
            Err(Error::ClaimCorrelation)
        };
    };
    let related = required_list(&claim.resource["related"], "Claim.related")?;
    if related.len() != 1
        || !concept(&related[0]["relationship"])
        || bundle.target(claim, &related[0]["claim"], "Claim")?.url != previous.url
    {
        return Err(Error::ClaimCorrelation);
    }
    request_claim_fields(
        previous.resource,
        previous.resource.get("related").is_some(),
    )?;
    for (key, kind) in [("patient", "Patient"), ("insurer", "Organization")] {
        if !std::ptr::eq(
            bundle.target(claim, &claim.resource[key], kind)?.resource,
            bundle
                .target(previous, &previous.resource[key], kind)?
                .resource,
        ) {
            return Err(Error::ClaimCorrelation);
        }
    }
    Ok(Some(previous))
}

fn certification_is(value: &Value, code: &str) -> Result<bool, Error> {
    Ok(
        extension(value, PAS, "extension-certificationType")?.is_some_and(|entry| {
            codings(&entry["valueCodeableConcept"])
                .any(|entry| entry == ("https://codesystem.x12.org/005010/1322", code))
        }),
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Approved,
    Denied,
    Pending,
    Modified,
    Cancelled,
}

fn action(item: &Value) -> Result<(Action, &Value), Error> {
    let mut review = None;
    for entry in required_list(&item["adjudication"], "ClaimResponse adjudication")? {
        if !concept(&entry["category"]) {
            return Err(Error::Profile("ClaimResponse.adjudication.category"));
        }
        if let Some(extension) = extension(entry, PAS, "extension-reviewAction")? {
            if review.replace(extension).is_some() {
                return Err(Error::ReviewAction);
            }
        }
    }
    let review = review.ok_or(Error::ReviewAction)?;
    let code = extension(review, PAS, "extension-reviewActionCode")?.ok_or(Error::ReviewAction)?;
    let mut codes =
        codings(&code["valueCodeableConcept"]).filter(|(system, _)| *system == ACTION_SYSTEM);
    let result = match codes.next().map(|(_, code)| code) {
        Some("A1") => Action::Approved,
        Some("A3") => Action::Denied,
        Some("A4") => Action::Pending,
        Some("A6") => Action::Modified,
        Some("C") => Action::Cancelled,
        _ => return Err(Error::ReviewAction),
    };
    if codes.next().is_some() {
        return Err(Error::ReviewAction);
    }
    Ok((result, review))
}

fn approval_number(response: &Value, review: &Value) -> Result<(), Error> {
    if extension(review, "", "number")?
        .and_then(|entry| required_text(&entry["valueString"]))
        .is_none()
        && required_text(&response["preAuthRef"]).is_none()
    {
        return Err(Error::ApprovalNumber);
    }
    Ok(())
}

fn same_party(
    prior: &Party<'_>,
    response: &Bundle<'_>,
    entry: Entry<'_>,
    key: &str,
    kind: &str,
) -> Result<(), Error> {
    let current = response.target(entry, &entry.resource[key], kind)?;
    if prior.url != current.url || *prior.id != current.resource["id"] {
        return Err(Error::ClaimCorrelation);
    }
    Ok(())
}

fn claim_matches(request: &Request<'_>, entry: Entry<'_>, bundle: &Bundle<'_>) -> bool {
    let reference = &entry.resource["request"];
    if reference.get("type").is_some()
        && !matches!(
            reference["type"].as_str(),
            Some("Claim" | "http://hl7.org/fhir/StructureDefinition/Claim")
        )
    {
        return false;
    }
    let identifier_matches = identifier(&reference["identifier"]).is_some_and(|target| {
        list(&request.claim["identifier"])
            .iter()
            .any(|prior| identifier(prior) == Some(target))
    });
    if reference.get("identifier").is_some() && !identifier_matches {
        return false;
    }
    if let Some(url) = reference.get("reference") {
        return required_text(url).is_some_and(|url| {
            if let Some(found) = bundle.resolve(entry, url) {
                return found.url == request.claim_url && found.resource == request.claim.as_ref();
            }
            let absolute_match = url == request.claim_url;
            let relative_match = rest_identity(entry.url)
                .zip(rest_identity(&request.claim_url))
                .is_some_and(|((from, _, _), (to, kind, id))| {
                    from == to && kind == "Claim" && url.strip_prefix("Claim/") == Some(id)
                });
            absolute_match || relative_match
        });
    }
    identifier_matches
}

pub struct Response {
    actions: BTreeMap<u32, Action>,
    business_error: bool,
    human_reviewed: Option<bool>,
}

impl Response {
    pub fn action(&self, sequence: u32) -> Option<Action> {
        self.actions.get(&sequence).copied()
    }

    pub fn business_error(&self) -> bool {
        self.business_error
    }

    /// A reported flag is useful for auditing, independently of verified reviewer credentials.
    pub fn human_reviewed(&self) -> Option<bool> {
        self.human_reviewed
    }
}

#[derive(Default)]
pub struct ResponseContext<'a> {
    /// Item sequences for which the payer requested more information. A4 alone can mean ordinary pending.
    pub additional_information_requested: &'a [u32],
}

pub fn validate_response(
    value: &Value,
    request: &Request<'_>,
    context: ResponseContext<'_>,
) -> Result<Response, Error> {
    let bundle = Bundle::collection(value, "profile-pas-response-bundle")?;
    let entry = bundle.entries[0];
    let response = entry.resource;
    profile(response, "ClaimResponse", "profile-claimresponse")?;
    if !claim_fields(response)
        || bundle
            .entries
            .iter()
            .skip(1)
            .any(|other| other.resource["resourceType"] == "ClaimResponse")
    {
        return Err(Error::Profile("profile-claimresponse"));
    }
    if !claim_matches(request, entry, &bundle) {
        return Err(Error::ClaimCorrelation);
    }
    same_party(&request.patient, &bundle, entry, "patient", "Patient")?;
    same_party(&request.insurer, &bundle, entry, "insurer", "Organization")?;
    patient(
        bundle
            .target(entry, &response["patient"], "Patient")?
            .resource,
        "profile-beneficiary",
    )?;
    organization(
        bundle
            .target(entry, &response["insurer"], "Organization")?
            .resource,
        "profile-insurer",
    )?;
    for resource in &bundle.entries {
        bundle.references(*resource, Some(&response["request"]), false)?;
    }
    let human_reviewed = extension(response, PAS, "extension-claimResponseReviewer")?
        .map(|reviewer| {
            extension(reviewer, "", "wasHumanReviewedFlag")?
                .and_then(|flag| flag["valueBoolean"].as_bool())
                .ok_or(Error::Profile("extension-claimResponseReviewer"))
        })
        .transpose()?;
    let mut actions = BTreeMap::new();
    if response["outcome"] == "error" {
        let errors = required_list(&response["error"], "ClaimResponse.error")?;
        if errors.iter().any(|error| {
            !concept(&error["code"])
                || error.get("itemSequence").is_some_and(|sequence| {
                    !positive(sequence).is_some_and(|number| request.items.contains_key(&number))
                })
        }) || ["item", "addItem", "communicationRequest"]
            .iter()
            .any(|key| response.get(key).is_some())
            || !context.additional_information_requested.is_empty()
        {
            return Err(Error::Profile("ClaimResponse.error"));
        }
        return Ok(Response {
            actions,
            business_error: true,
            human_reviewed,
        });
    }
    if !matches!(response["outcome"].as_str(), Some("complete" | "partial")) {
        return Err(Error::Profile("ClaimResponse.outcome"));
    }
    let response_items = if request.items.is_empty() && response.get("item").is_none() {
        let (result, _) = action(response)?;
        if result != Action::Cancelled {
            return Err(Error::ReviewAction);
        }
        &[]
    } else {
        required_list(&response["item"], "ClaimResponse.item")?
    };
    for item in response_items {
        let sequence = positive(&item["itemSequence"]).ok_or(Error::ItemCorrelation)?;
        let (result, review) = action(item)?;
        if !request.items.contains_key(&sequence) || actions.insert(sequence, result).is_some() {
            return Err(Error::ItemCorrelation);
        }
        if result == Action::Approved {
            approval_number(response, review)?;
        }
        if result == Action::Denied
            && !list(&review["extension"]).iter().any(|reason| {
                reason["url"] == "reasonCode" && concept(&reason["valueCodeableConcept"])
            })
            && !denial_note(item, response)
        {
            return Err(Error::DenialReason);
        }
    }
    if actions.len() != request.items.len() {
        return Err(Error::ItemCorrelation);
    }
    let mut replacements = BTreeSet::new();
    for added in optional_list(response, "addItem")? {
        if !concept(&added["productOrService"]) {
            return Err(Error::Modification);
        }
        let (result, review) = action(added)?;
        if result != Action::Approved {
            return Err(Error::Modification);
        }
        approval_number(response, review)?;
        let mut linked = BTreeSet::new();
        for link in optional_list(added, "itemSequence")? {
            let sequence = positive(link).ok_or(Error::Modification)?;
            if !linked.insert(sequence) {
                return Err(Error::Modification);
            }
            if actions
                .get(&sequence)
                .is_some_and(|action| *action != Action::Modified)
            {
                return Err(Error::Modification);
            }
            if actions.contains_key(&sequence) {
                replacements.insert(sequence);
            }
        }
        if linked.iter().any(|sequence| actions.contains_key(sequence))
            && linked
                .iter()
                .any(|sequence| !actions.contains_key(sequence))
        {
            return Err(Error::Modification);
        }
    }
    if actions
        .iter()
        .any(|(sequence, action)| *action == Action::Modified && !replacements.contains(sequence))
    {
        return Err(Error::Modification);
    }
    validate_information_requests(response, entry, &bundle, &actions, context)?;
    Ok(Response {
        actions,
        business_error: false,
        human_reviewed,
    })
}

fn denial_note(item: &Value, response: &Value) -> bool {
    list(&item["noteNumber"])
        .iter()
        .filter_map(positive)
        .any(|number| {
            list(&response["processNote"]).iter().any(|note| {
                positive(&note["number"]) == Some(number) && required_text(&note["text"]).is_some()
            })
        })
}

fn validate_information_requests(
    response: &Value,
    entry: Entry<'_>,
    bundle: &Bundle<'_>,
    actions: &BTreeMap<u32, Action>,
    context: ResponseContext<'_>,
) -> Result<(), Error> {
    let mut requested = BTreeSet::new();
    let mut claim_wide = false;
    let mut seen = BTreeSet::new();
    for reference in optional_list(response, "communicationRequest")? {
        let communication = bundle.target(entry, reference, "CommunicationRequest")?;
        if !seen.insert((communication.url, communication.resource["id"].as_str())) {
            return Err(Error::PendingInformation);
        }
        let resource = communication.resource;
        profile(
            resource,
            "CommunicationRequest",
            "profile-communicationrequest",
        )?;
        if resource["status"] != "active" {
            return Err(Error::PendingInformation);
        }
        let resource_line = extension(resource, PAS, "extension-serviceLineNumber")?;
        for payload in required_list(&resource["payload"], "CommunicationRequest.payload")? {
            if required_text(&payload["contentString"]).is_none()
                || payload.get("contentReference").is_some()
                || payload.get("contentAttachment").is_some()
            {
                return Err(Error::PendingInformation);
            }
            let payload_line = extension(payload, PAS, "extension-serviceLineNumber")?;
            if let (Some(left), Some(right)) = (resource_line, payload_line) {
                if left["valuePositiveInt"] != right["valuePositiveInt"] {
                    return Err(Error::PendingInformation);
                }
            }
            if let Some(line) = resource_line.or(payload_line) {
                let sequence =
                    positive(&line["valuePositiveInt"]).ok_or(Error::PendingInformation)?;
                if actions.get(&sequence) != Some(&Action::Pending) {
                    return Err(Error::PendingInformation);
                }
                requested.insert(sequence);
            } else {
                claim_wide = true;
            }
        }
    }
    if claim_wide && !actions.values().any(|action| *action == Action::Pending) {
        return Err(Error::PendingInformation);
    }
    if context
        .additional_information_requested
        .iter()
        .any(|sequence| {
            actions.get(sequence) != Some(&Action::Pending)
                || (!claim_wide && !requested.contains(sequence))
        })
    {
        return Err(Error::PendingInformation);
    }
    if bundle.entries.iter().any(|entry| {
        entry.resource["resourceType"] == "CommunicationRequest"
            && !seen.contains(&(entry.url, entry.resource["id"].as_str()))
    }) {
        return Err(Error::PendingInformation);
    }
    Ok(())
}

pub fn validate_subscription(value: &Value, sending_systems: &[&str]) -> Result<(), Error> {
    profile(value, "Subscription", "profile-subscription")?;
    let channel = &value["channel"];
    let content = extension(&channel["_payload"], BACKPORT, "backport-payload-content")?;
    if !matches!(
        value["status"].as_str(),
        Some("requested" | "active" | "error" | "off")
    ) || required_text(&value["reason"]).is_none()
        || value["criteria"] != TOPIC
        || channel["type"] != "rest-hook"
        || !required_text(&channel["endpoint"]).is_some_and(rest_url)
        || !matches!(
            channel["payload"].as_str(),
            Some("application/fhir+json" | "application/fhir+xml")
        )
        || !content.is_some_and(|extension| extension["valueCode"] == "full-resource")
    {
        return Err(Error::Subscription);
    }
    let filters = required_list(&value["_criteria"]["extension"], "Subscription filter")?;
    let mut seen = BTreeSet::new();
    for filter in filters
        .iter()
        .filter(|entry| entry["url"] == format!("{BACKPORT}backport-filter-criteria"))
    {
        let ids = required_text(&filter["valueString"])
            .and_then(|value| value.strip_prefix("org-identifier="))
            .ok_or(Error::Subscription)?;
        for id in ids.split(',') {
            if id.is_empty() || !sending_systems.contains(&id) || !seen.insert(id) {
                return Err(Error::Subscription);
            }
        }
    }
    (!seen.is_empty()).then_some(()).ok_or(Error::Subscription)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Inquiry {
    Manual,
    AutomatedPolling,
}

pub fn validate_inquiry(inquiry: Inquiry) -> Result<(), Error> {
    (inquiry == Inquiry::Manual)
        .then_some(())
        .ok_or(Error::Subscription)
}

fn parameter<'a>(values: &'a [Value], name: &str) -> Result<&'a Value, Error> {
    let mut entries = values.iter().filter(|value| value["name"] == name);
    let entry = entries.next().ok_or(Error::Notification)?;
    if entries.next().is_some() {
        return Err(Error::Notification);
    }
    Ok(entry)
}

/// Validate a subscription notification and every full PAS response it points to.
pub fn validate_notification(
    value: &Value,
    subscription_reference: &str,
    mut validate_focus: impl FnMut(&Value) -> Result<(), Error>,
) -> Result<(), Error> {
    if value["resourceType"] != "Bundle"
        || value["type"] != "history"
        || !instant(&value["timestamp"])
    {
        return Err(Error::Notification);
    }
    let entries = required_list(&value["entry"], "Notification entries")?;
    let status = &entries[0]["resource"];
    if status["resourceType"] != "Parameters"
        || subscription_reference.trim().is_empty()
        || !required_text(&entries[0]["fullUrl"]).is_some_and(full_url)
    {
        return Err(Error::Notification);
    }
    let parameters = required_list(&status["parameter"], "Notification parameters")?;
    if parameter(parameters, "topic")?["valueCanonical"] != TOPIC
        || parameter(parameters, "subscription")?["valueReference"]["reference"]
            != subscription_reference
    {
        return Err(Error::Notification);
    }
    let event_parameters = parameters
        .iter()
        .filter(|parameter| parameter["name"] == "notification-event");
    match parameter(parameters, "type")?["valueCode"].as_str() {
        Some("handshake" | "heartbeat") => {
            return (entries.len() == 1 && event_parameters.count() == 0)
                .then_some(())
                .ok_or(Error::Notification);
        }
        Some("event-notification") => {}
        _ => return Err(Error::Notification),
    }
    let mut payloads = BTreeMap::new();
    for entry in &entries[1..] {
        let url = required_text(&entry["fullUrl"])
            .filter(|url| full_url(url))
            .ok_or(Error::Notification)?;
        let payload = &entry["resource"];
        if entries[0]["fullUrl"] == url || payloads.insert(url, payload).is_some() {
            return Err(Error::Notification);
        }
    }
    let mut events = BTreeSet::new();
    for event in event_parameters {
        let parts = required_list(&event["part"], "Notification event parts")?;
        let number = required_text(&parameter(parts, "event-number")?["valueString"])
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|number| *number > 0)
            .ok_or(Error::Notification)?;
        if !events.insert(number) {
            return Err(Error::Notification);
        }
        let reference = required_text(&parameter(parts, "focus")?["valueReference"]["reference"])
            .ok_or(Error::Notification)?;
        let payload = payloads.remove(reference).ok_or(Error::Notification)?;
        Bundle::collection(payload, "profile-pas-response-bundle")?;
        validate_focus(payload)?;
    }
    (!events.is_empty() && payloads.is_empty())
        .then_some(())
        .ok_or(Error::Notification)
}
