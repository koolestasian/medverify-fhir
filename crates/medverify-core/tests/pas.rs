use jiff::{civil::date, Timestamp, Zoned};
use medverify_core::fhir_pas::*;
use serde_json::{json, Value};

const PAS: &str = "http://hl7.org/fhir/us/davinci-pas/StructureDefinition/";
const BACKPORT: &str = "http://hl7.org/fhir/uv/subscriptions-backport/StructureDefinition/";
const TOPIC: &str = "http://hl7.org/fhir/us/davinci-pas/SubscriptionTopic/PASSubscriptionTopic";
const MEDICAL: &str = "https://example.test/synthetic-procedures";

fn request() -> Value {
    parse_json(include_str!("fixtures/pas/request.json")).unwrap()
}

fn response() -> Value {
    parse_json(include_str!("fixtures/pas/response.json")).unwrap()
}

fn set_action(line: &mut Value, code: &str) {
    line["adjudication"][0]["extension"][0]["extension"][0]["valueCodeableConcept"]["coding"][0]
        ["code"] = json!(code);
}

fn medical(system: &str, _: &str) -> Option<ServiceKind> {
    (system == MEDICAL).then_some(ServiceKind::Medical)
}

#[test]
fn collection_validates_profiles_and_inline_references() {
    let mut value = request();
    let parsed = validate_request(&value).unwrap();
    assert_eq!(parsed.priority(), RequestPriority::Standard);
    assert_eq!(
        parsed.items().map(|item| item.sequence).collect::<Vec<_>>(),
        [1, 5]
    );

    value["entry"][0]["resource"]["extension"] = json!([{
        "url": format!("{PAS}extension-levelOfServiceCode"),
        "valueCodeableConcept": {"coding":[{"system":"https://codesystem.x12.org/005010/1338","code":"U"}]}
    }]);
    assert_eq!(
        validate_request(&value).unwrap().priority(),
        RequestPriority::Expedited
    );

    let mut malformed = value.clone();
    malformed["type"] = json!("transaction");
    assert!(matches!(validate_request(&malformed), Err(Error::Bundle)));
    malformed = value.clone();
    malformed["entry"].as_array_mut().unwrap().swap(0, 1);
    assert!(matches!(
        validate_request(&malformed),
        Err(Error::Profile(_))
    ));
    malformed = value.clone();
    malformed["entry"][0]["resource"]["use"] = json!("claim");
    assert!(matches!(
        validate_request(&malformed),
        Err(Error::Profile(_))
    ));
    malformed = value.clone();
    malformed["entry"][0]["resource"]["meta"]["profile"][0] =
        json!(format!("{PAS}profile-claim|2.1.0"));
    assert!(matches!(
        validate_request(&malformed),
        Err(Error::Profile(_))
    ));
    malformed = value.clone();
    malformed["entry"][0]["resource"]["item"][0]["extension"][1]["valueCodeableConcept"] =
        json!({"text":"initial"});
    assert!(matches!(
        validate_request(&malformed),
        Err(Error::Profile(_))
    ));
    malformed = value.clone();
    malformed["entry"][4]["resource"]["identifier"][0]["type"] = json!({"text":"Policy number"});
    assert!(matches!(
        validate_request(&malformed),
        Err(Error::Profile(_))
    ));
    malformed = value.clone();
    malformed["entry"][0]["resource"]["item"][0]["informationSequence"] = json!([1]);
    assert!(matches!(
        validate_request(&malformed),
        Err(Error::ItemCorrelation)
    ));
    malformed = value.clone();
    malformed["entry"][0]["resource"]["patient"]["reference"] = json!("Organization/payer");
    assert!(matches!(
        validate_request(&malformed),
        Err(Error::ReferenceType)
    ));
    malformed = value.clone();
    malformed["entry"][1]["fullUrl"] = json!("https://another.test/fhir/Patient/patient");
    assert!(matches!(
        validate_request(&malformed),
        Err(Error::Reference)
    ));
    let duplicate = value["entry"][1].clone();
    value["entry"].as_array_mut().unwrap().push(duplicate);
    assert!(matches!(
        validate_request(&value),
        Err(Error::DuplicateResource)
    ));
}

#[test]
fn references_resolve_through_extensions_and_match_versioned_and_urn_targets() {
    let mut value = request();
    value["entry"][0]["resource"]["item"][0]["extension"].as_array_mut().unwrap().push(
        json!({"url":format!("{PAS}extension-requestedService"),"valueReference":{"reference":"ServiceRequest/synthetic/_history/2"}})
    );
    value["entry"].as_array_mut().unwrap().push(json!({
        "fullUrl":"https://example.test/fhir/ServiceRequest/synthetic",
        "resource":{
            "resourceType":"ServiceRequest","id":"synthetic","meta":{"versionId":"2"},
            "status":"active","intent":"order","subject":{"reference":"Patient/patient"}
        }
    }));
    assert!(validate_request(&value).is_ok());
    value["entry"][5]["resource"]["meta"]["versionId"] = json!("3");
    assert!(matches!(validate_request(&value), Err(Error::Reference)));

    let mut urns = request();
    let patient = "urn:uuid:557a5ba5-8ac5-4b42-a02b-e01cc23852c9";
    urns["entry"][1]["fullUrl"] = json!(patient);
    urns["entry"][0]["resource"]["patient"]["reference"] = json!(patient);
    urns["entry"][4]["resource"]["beneficiary"]["reference"] = json!(patient);
    assert!(validate_request(&urns).is_ok());
    urns["entry"][4]["resource"]["beneficiary"]["reference"] = json!("Patient/patient");
    assert!(matches!(validate_request(&urns), Err(Error::Reference)));
}

#[test]
fn claim_update_requires_its_previous_claim_and_preserves_line_numbers() {
    let mut value = request();
    let mut previous = value["entry"][0].clone();
    previous["fullUrl"] = json!("https://example.test/fhir/Claim/previous");
    previous["resource"]["id"] = json!("previous");
    value["entry"][0]["resource"]["meta"]["profile"][0] =
        json!(format!("{PAS}profile-claim-update"));
    value["entry"][0]["resource"]["related"] = json!([{
        "claim":{"reference":"Claim/previous"},
        "relationship":{"coding":[{"system":"http://terminology.hl7.org/CodeSystem/ex-relatedclaimrelationship","code":"prior"}]}
    }]);
    assert!(matches!(
        validate_request(&value),
        Err(Error::ClaimCorrelation)
    ));
    value["entry"].as_array_mut().unwrap().push(previous);
    assert!(validate_request(&value).is_ok());
    value["entry"][0]["resource"]["item"]
        .as_array_mut()
        .unwrap()
        .pop();
    assert!(matches!(
        validate_request(&value),
        Err(Error::ItemCorrelation)
    ));
}

#[test]
fn response_matches_claim_by_reference_or_identifier_and_correlates_original_lines() {
    let submission = request();
    let request = validate_request(&submission).unwrap();
    let mut value = response();
    let parsed = validate_response(&value, &request, ResponseContext::default()).unwrap();
    assert_eq!(parsed.action(1), Some(Action::Approved));
    assert_eq!(parsed.action(5), Some(Action::Approved));
    assert_eq!(parsed.action(2), None);
    value["entry"][0]["resource"]["request"] = json!({
        "identifier":{"system":"https://example.test/identifiers","value":"prior-auth-request"}
    });
    assert!(validate_response(&value, &request, ResponseContext::default()).is_ok());
    let mut wrong = value.clone();
    wrong["entry"][0]["resource"]["request"]["identifier"]["value"] = json!("other-request");
    assert!(matches!(
        validate_response(&wrong, &request, ResponseContext::default()),
        Err(Error::ClaimCorrelation)
    ));
    wrong = value.clone();
    wrong["entry"][0]["resource"]["item"][0]["itemSequence"] = json!(1);
    assert!(matches!(
        validate_response(&wrong, &request, ResponseContext::default()),
        Err(Error::ItemCorrelation)
    ));
    wrong = value.clone();
    wrong["entry"][0]["resource"]["item"][0]["adjudication"][0]["extension"][0]["extension"]
        .as_array_mut()
        .unwrap()
        .pop();
    assert!(matches!(
        validate_response(&wrong, &request, ResponseContext::default()),
        Err(Error::ApprovalNumber)
    ));
    wrong = value.clone();
    wrong["entry"].as_array_mut().unwrap().pop();
    assert!(matches!(
        validate_response(&wrong, &request, ResponseContext::default()),
        Err(Error::Reference)
    ));

    value["entry"][0]["resource"]["outcome"] = json!("error");
    value["entry"][0]["resource"]
        .as_object_mut()
        .unwrap()
        .remove("item");
    // ClaimResponse.error.code has a required binding: text alone does not conform.
    value["entry"][0]["resource"]["error"] = json!([{"code":{"text":"Invalid transaction"}}]);
    assert!(matches!(
        validate_response(&value, &request, ResponseContext::default()),
        Err(Error::Profile("ClaimResponse.error"))
    ));
    value["entry"][0]["resource"]["error"] = json!([{"code":{
        "coding":[{"system":"https://codesystem.x12.org/005010/901","code":"15"}],
        "text":"Invalid transaction"
    }}]);
    assert!(
        validate_response(&value, &request, ResponseContext::default())
            .unwrap()
            .business_error()
    );
}

#[test]
fn denial_requires_a_specific_coded_reason_or_a_line_linked_note() {
    let submission = request();
    let request = validate_request(&submission).unwrap();
    let mut value = response();
    set_action(&mut value["entry"][0]["resource"]["item"][1], "A3");
    assert!(matches!(
        validate_response(&value, &request, ResponseContext::default()),
        Err(Error::DenialReason)
    ));
    value["entry"][0]["resource"]["item"][1]["noteNumber"] = json!([7]);
    value["entry"][0]["resource"]["processNote"] = json!([{
        "number":7,"text":"The request does not document the required baseline assessment."
    }]);
    value["entry"][0]["resource"]["extension"] = json!([{
        "url":format!("{PAS}extension-claimResponseReviewer"),
        "extension":[{"url":"wasHumanReviewedFlag","valueBoolean":true}]
    }]);
    let result = validate_response(&value, &request, ResponseContext::default()).unwrap();
    assert_eq!(result.action(1), Some(Action::Denied));
    assert_eq!(result.human_reviewed(), Some(true));
    value["entry"][0]["resource"]["processNote"][0]["number"] = json!(8);
    assert!(matches!(
        validate_response(&value, &request, ResponseContext::default()),
        Err(Error::DenialReason)
    ));

    // reasonCode has a required binding to X12278ReviewDecisionReasonCode (PAS 2.2.1), which is
    // X12 external code list 886 and nothing else. Text, a code without a system and an invented
    // local system/code are not members, so the denial has no valid reason.
    let x12 = |code: &str| json!({"coding":[{"system":"https://codesystem.x12.org/external/886","code":code}]});
    let licensed = |code: &str| code == "0U";
    let with_codes = || ResponseContext {
        decision_reason_codes: Some(&licensed),
        ..ResponseContext::default()
    };
    let not_member = Some(Error::Profile("extension-reviewAction.reasonCode"));
    for reason in [
        json!({"text":"Not medically necessary"}),
        json!({"coding":[{"code":"baseline-missing"}]}),
        json!({"coding":[{"system":"https://example.test/denial-reason","code":"baseline-missing"}]}),
        json!({"coding":[{"system":"https://codesystem.x12.org/005010/886","code":"0U"}]}),
    ] {
        reasons_of(&mut value).push(json!({"url":"reasonCode","valueCodeableConcept":reason}));
        for context in [ResponseContext::default(), with_codes()] {
            assert_eq!(
                validate_response(&value, &request, context).err(),
                not_member,
                "{reason}"
            );
        }
        reasons_of(&mut value).pop();
    }

    // An X12 code: its membership needs the operator's licensed code list, so without one there
    // is no verdict; with one, membership decides.
    reasons_of(&mut value).push(json!({"url":"reasonCode","valueCodeableConcept":x12("0U")}));
    assert_eq!(
        validate_response(&value, &request, ResponseContext::default()).err(),
        Some(Error::TerminologyUnavailable(
            "X12278ReviewDecisionReasonCode"
        ))
    );
    let result = validate_response(&value, &request, with_codes()).unwrap();
    assert_eq!(result.action(1), Some(Action::Denied));
    reasons_of(&mut value).pop();
    reasons_of(&mut value).push(json!({"url":"reasonCode","valueCodeableConcept":x12("ZZ")}));
    assert_eq!(
        validate_response(&value, &request, with_codes()).err(),
        Some(Error::Profile("extension-reviewAction.reasonCode"))
    );
    // A translation beside the X12 code is allowed; an invented code cannot ride along on a note.
    reasons_of(&mut value).pop();
    let mut translated = x12("0U");
    translated["coding"]
        .as_array_mut()
        .unwrap()
        .push(json!({"system":"https://example.test/denial-reason","code":"baseline-missing"}));
    reasons_of(&mut value).push(json!({"url":"reasonCode","valueCodeableConcept":translated}));
    assert!(validate_response(&value, &request, with_codes()).is_ok());
    reasons_of(&mut value).pop();
    value["entry"][0]["resource"]["processNote"][0]["number"] = json!(7);
    assert!(validate_response(&value, &request, ResponseContext::default()).is_ok());
    reasons_of(&mut value).push(json!({
        "url":"reasonCode", "valueCodeableConcept":{"coding":[{"system":"https://example.test/denial-reason","code":"baseline-missing"}]}
    }));
    assert_eq!(
        validate_response(&value, &request, with_codes()).err(),
        not_member
    );
}

#[test]
fn review_reasons_are_checked_wherever_a_review_action_appears() {
    // The binding applies to approvals and pended items as well as denials.
    let submission = request();
    let request = validate_request(&submission).unwrap();
    let mut value = response();
    let review = &mut value["entry"][0]["resource"]["item"][0]["adjudication"][0]["extension"][0];
    review["extension"].as_array_mut().unwrap().push(json!({
        "url":"reasonCode",
        "valueCodeableConcept":{"coding":[{"system":"https://example.test/approval-reason","code":"ok"}]}
    }));
    assert_eq!(
        validate_response(&value, &request, ResponseContext::default()).err(),
        Some(Error::Profile("extension-reviewAction.reasonCode"))
    );
    assert_eq!(
        validate_response(&response(), &request, ResponseContext::default()).err(),
        None
    );
}

fn reasons_of(value: &mut Value) -> &mut Vec<Value> {
    value["entry"][0]["resource"]["item"][1]["adjudication"][0]["extension"][0]["extension"]
        .as_array_mut()
        .unwrap()
}

#[test]
fn pending_for_additional_information_requires_a_linked_communication_request() {
    let submission = request();
    let request = validate_request(&submission).unwrap();
    let mut value = response();
    set_action(&mut value["entry"][0]["resource"]["item"][1], "A4");
    assert_eq!(
        validate_response(&value, &request, ResponseContext::default())
            .unwrap()
            .action(1),
        Some(Action::Pending)
    );
    let context = || ResponseContext {
        additional_information_requested: &[1],
        ..ResponseContext::default()
    };
    assert!(matches!(
        validate_response(&value, &request, context()),
        Err(Error::PendingInformation)
    ));
    value["entry"][0]["resource"]["communicationRequest"] =
        json!([{"reference":"CommunicationRequest/more"}]);
    value["entry"].as_array_mut().unwrap().push(json!({
        "fullUrl":"https://example.test/fhir/CommunicationRequest/more",
        "resource":{
            "resourceType":"CommunicationRequest","id":"more","status":"active",
            "extension":[{"url":format!("{PAS}extension-serviceLineNumber"),"valuePositiveInt":1}],
            "payload":[{"contentString":"Provide the baseline assessment."}]
        }
    }));
    assert!(validate_response(&value, &request, context()).is_ok());
    value["entry"][3]["resource"]["extension"][0]["valuePositiveInt"] = json!(5);
    assert!(matches!(
        validate_response(&value, &request, context()),
        Err(Error::PendingInformation)
    ));
    let communication = &mut value["entry"][3]["resource"];
    let misplaced = communication
        .as_object_mut()
        .unwrap()
        .remove("extension")
        .unwrap();
    communication["payload"][0]["extension"] = misplaced;
    assert!(matches!(
        validate_response(&value, &request, context()),
        Err(Error::PendingInformation)
    ));
    value["entry"][3]["resource"]["payload"][0]["extension"][0]["valuePositiveInt"] = json!(1);
    assert!(validate_response(&value, &request, context()).is_ok());
}

#[test]
fn modified_items_require_authorized_details_linked_to_the_original_sequence() {
    let submission = request();
    let request = validate_request(&submission).unwrap();
    let mut value = response();
    set_action(&mut value["entry"][0]["resource"]["item"][1], "A6");
    assert!(matches!(
        validate_response(&value, &request, ResponseContext::default()),
        Err(Error::Modification)
    ));
    let authorized = value["entry"][0]["resource"]["item"][0]["adjudication"].clone();
    value["entry"][0]["resource"]["addItem"] = json!([{
        "itemSequence":[1], "adjudication":authorized,
        "productOrService":{"coding":[{"system":MEDICAL,"code":"Y9016"}]}
    }]);
    assert_eq!(
        validate_response(&value, &request, ResponseContext::default())
            .unwrap()
            .action(1),
        Some(Action::Modified)
    );
    value["entry"][0]["resource"]["addItem"][0]["itemSequence"] = json!([5]);
    assert!(matches!(
        validate_response(&value, &request, ResponseContext::default()),
        Err(Error::Modification)
    ));
}

fn zoned(time: &str) -> Zoned {
    time.parse().unwrap()
}

fn ts(time: &str) -> Timestamp {
    time.parse().unwrap()
}

fn timeline(received: &Zoned, plan: Plan, priority: Priority) -> Timeline<'_> {
    Timeline {
        payer: Payer {
            plan,
            rating_period_start: Some(date(2026, 1, 1)),
        },
        received,
        priority,
        checked_at: ts("2026-12-01T00:00:00Z"),
        extension: None,
        earlier_deadline: None,
    }
}

#[test]
fn cms_rule_excludes_drug_items_individually_and_applies_to_covered_payers() {
    let received = zoned("2026-07-06T09:00:00-07:00[America/Los_Angeles]");
    let submission = request();
    let request = validate_request(&submission).unwrap();
    let items: Vec<_> = request.items().collect();
    let timeline = timeline(&received, Plan::MedicareAdvantage, Priority::Standard);
    let event = Some(CommunicatedAction {
        action: Action::Approved,
        at: ts("2026-07-07T09:00:00-07:00"),
    });
    assert!(matches!(
        timeline.assess_item(items[0], medical, event),
        Ok(Timeliness::OnTime { .. })
    ));
    assert_eq!(
        timeline.assess_item(items[1], |_, _| unreachable!(), event),
        Ok(Timeliness::OutsideScope(TimingScope::DrugExcluded))
    );
    assert_eq!(
        timeline.assess_item(items[0], |_, _| None, event),
        Ok(Timeliness::OutsideScope(TimingScope::UnclassifiedService))
    );

    for plan in [
        Plan::OriginalMedicare,
        Plan::FederalExchangeQhp,
        Plan::StateRegulatedCommercial,
        Plan::SelfFundedErisa,
    ] {
        let payer = Payer {
            plan,
            rating_period_start: None,
        };
        assert_eq!(
            payer.timing_scope(date(2026, 7, 6), Some(ServiceKind::Medical)),
            Ok(TimingScope::PayerExcluded)
        );
    }
    for plan in [
        Plan::MedicareAdvantage,
        Plan::ApplicableIntegratedPlan,
        Plan::MedicaidFfs,
        Plan::MedicaidManagedCare,
        Plan::ChipFfs,
        Plan::ChipManagedCare,
    ] {
        let payer = Payer {
            plan,
            rating_period_start: Some(date(2026, 3, 1)),
        };
        assert_eq!(
            payer.timing_scope(date(2026, 3, 1), Some(ServiceKind::Medical)),
            Ok(TimingScope::Applies)
        );
        assert_eq!(
            payer.timing_scope(date(2025, 12, 31), Some(ServiceKind::Medical)),
            Ok(TimingScope::BeforeEffectiveDate)
        );
    }
    let payer = Payer {
        plan: Plan::MedicaidManagedCare,
        rating_period_start: Some(date(2025, 8, 1)),
    };
    assert_eq!(
        payer.timing_scope(date(2026, 7, 6), Some(ServiceKind::Medical)),
        Ok(TimingScope::BeforeEffectiveDate)
    );
}

#[test]
fn expedited_uses_elapsed_hours_and_standard_uses_local_calendar_days_through_dst() {
    for (received, expedited, standard) in [
        (
            "2026-03-06T09:00:00-08:00[America/Los_Angeles]",
            "2026-03-09T10:00:00-07:00",
            "2026-03-13T09:00:00-07:00",
        ),
        (
            "2026-10-30T09:00:00-07:00[America/Los_Angeles]",
            "2026-11-02T08:00:00-08:00",
            "2026-11-06T09:00:00-08:00",
        ),
    ] {
        let received = zoned(received);
        for (priority, boundary) in [
            (Priority::Expedited, expedited),
            (Priority::Standard, standard),
        ] {
            let timeline = timeline(&received, Plan::MedicareAdvantage, priority);
            let deadline = ts(boundary);
            let just_late = deadline
                .checked_add(jiff::SignedDuration::from_nanos(1))
                .unwrap();
            let result = |at| {
                timeline.assess(
                    Some(ServiceKind::Medical),
                    Some(CommunicatedAction {
                        action: Action::Approved,
                        at,
                    }),
                )
            };
            assert_eq!(result(deadline), Ok(Timeliness::OnTime { deadline }));
            assert_eq!(result(just_late), Ok(Timeliness::Late { deadline }));
            let pending = Some(CommunicatedAction {
                action: Action::Pending,
                at: deadline,
            });
            assert_eq!(
                Timeline {
                    checked_at: deadline,
                    ..timeline
                }
                .assess(Some(ServiceKind::Medical), pending),
                Ok(Timeliness::AwaitingDecision { deadline })
            );
            assert_eq!(
                timeline.assess(Some(ServiceKind::Medical), pending),
                Ok(Timeliness::Overdue { deadline })
            );
        }
    }
}

#[test]
fn extensions_depend_on_payer_priority_reason_notice_and_elapsed_time() {
    let received = zoned("2026-10-30T09:00:00-07:00[America/Los_Angeles]");
    let timeline = timeline(&received, Plan::MedicareAdvantage, Priority::Expedited);
    let granted_at = ts("2026-11-01T09:00:00-08:00");
    let extension = Extension {
        calendar_days: 14,
        granted_at,
        reason: ExtensionReason::EnrolleeRequested,
        notice: Some(ExtensionNotice {
            sent_at: granted_at,
            written_reason: true,
            grievance_rights: Some(GrievanceRights::Expedited),
        }),
    };
    let assess = |timeline: Timeline<'_>, extension| {
        Timeline {
            extension: Some(extension),
            ..timeline
        }
        .assess(Some(ServiceKind::Medical), None)
    };
    let deadline = ts("2026-11-16T08:00:00-08:00");
    assert_eq!(
        assess(timeline, extension),
        Ok(Timeliness::Overdue { deadline })
    );
    assert_eq!(
        assess(
            timeline,
            Extension {
                calendar_days: 15,
                ..extension
            }
        ),
        Err(TimingError::InvalidExtension)
    );
    assert_eq!(
        assess(
            timeline,
            Extension {
                notice: None,
                ..extension
            }
        ),
        Err(TimingError::MissingExtensionNotice)
    );
    assert_eq!(
        assess(
            timeline,
            Extension {
                granted_at: ts("2026-11-02T08:00:01-08:00"),
                ..extension
            }
        ),
        Err(TimingError::InvalidExtension)
    );
    let ffs = Timeline {
        payer: Payer {
            plan: Plan::MedicaidFfs,
            ..timeline.payer
        },
        ..timeline
    };
    assert_eq!(assess(ffs, extension), Err(TimingError::InvalidExtension));
    let standard = Timeline {
        priority: Priority::Standard,
        ..ffs
    };
    let extension = Extension {
        reason: ExtensionReason::ProviderRequested,
        notice: None,
        ..extension
    };
    assert_eq!(
        assess(standard, extension),
        Ok(Timeliness::Overdue {
            deadline: ts("2026-11-20T09:00:00-08:00")
        })
    );
    let managed = Timeline {
        payer: Payer {
            plan: Plan::MedicaidManagedCare,
            ..standard.payer
        },
        ..standard
    };
    assert_eq!(
        assess(managed, extension),
        Err(TimingError::MissingExtensionNotice)
    );
    let managed_notice = ExtensionNotice {
        sent_at: granted_at,
        written_reason: true,
        grievance_rights: Some(GrievanceRights::Standard),
    };
    assert_eq!(
        assess(
            managed,
            Extension {
                notice: Some(managed_notice),
                ..extension
            }
        ),
        Ok(Timeliness::Overdue {
            deadline: ts("2026-11-20T09:00:00-08:00")
        })
    );
    let early = ts("2026-11-02T00:00:00-08:00");
    assert_eq!(
        assess(
            Timeline {
                earlier_deadline: Some(early),
                ..standard
            },
            extension
        ),
        Ok(Timeliness::Overdue { deadline: early })
    );
}

#[test]
fn rest_hook_subscription_is_scoped_to_its_sending_system_and_uses_full_resources() {
    let mut subscription = json!({
        "resourceType":"Subscription","meta":{"profile":[format!("{PAS}profile-subscription")]},
        "status":"requested", "reason":"Receive changes to prior authorizations.", "criteria":TOPIC,
        "_criteria":{"extension":[{"url":format!("{BACKPORT}backport-filter-criteria"),"valueString":"org-identifier=sample-clinic"}]},
        "channel":{"type":"rest-hook","endpoint":"https://example.test/callback","payload":"application/fhir+json",
            "_payload":{"extension":[{"url":format!("{BACKPORT}backport-payload-content"),"valueCode":"full-resource"}]}}
    });
    assert_eq!(
        validate_subscription(&subscription, &["sample-clinic"]),
        Ok(())
    );
    assert_eq!(
        validate_subscription(&subscription, &["different-clinic"]),
        Err(Error::Subscription)
    );
    subscription["channel"]["_payload"]["extension"][0]["valueCode"] = json!("id-only");
    assert_eq!(
        validate_subscription(&subscription, &["sample-clinic"]),
        Err(Error::Subscription)
    );
    assert_eq!(validate_inquiry(Inquiry::Manual), Ok(()));
    assert_eq!(
        validate_inquiry(Inquiry::AutomatedPolling),
        Err(Error::Subscription)
    );
}

#[test]
fn event_notification_correlates_subscription_and_embeds_a_complete_pas_response() {
    let request_value = request();
    let request = validate_request(&request_value).unwrap();
    let mut event = json!({
        "resourceType":"Bundle","type":"history","timestamp":"2026-07-06T09:07:00-07:00",
        "entry":[
            {"fullUrl":"https://example.test/fhir/Parameters/status","resource":{
                "resourceType":"Parameters","parameter":[
                    {"name":"topic","valueCanonical":TOPIC},
                    {"name":"type","valueCode":"event-notification"},
                    {"name":"subscription","valueReference":{"reference":"Subscription/notify"}},
                    {"name":"notification-event","part":[
                        {"name":"event-number","valueString":"1"},
                        {"name":"focus","valueReference":{"reference":"https://example.test/fhir/Bundle/response"}}
                    ]}
                ]
            }},
            {"fullUrl":"https://example.test/fhir/Bundle/response","resource":response()}
        ]
    });
    let check_response = |payload: &Value| {
        validate_response(payload, &request, ResponseContext::default()).map(|_| ())
    };
    assert_eq!(
        validate_notification(&event, "Subscription/notify", check_response),
        Ok(())
    );
    assert_eq!(
        validate_notification(&event, "Subscription/different", check_response),
        Err(Error::Notification)
    );
    event["entry"][1]["resource"]["entry"] = json!([]);
    assert!(validate_notification(&event, "Subscription/notify", check_response).is_err());
    event["entry"].as_array_mut().unwrap().pop();
    event["entry"][0]["resource"]["parameter"]
        .as_array_mut()
        .unwrap()
        .pop();
    for notification in ["handshake", "heartbeat"] {
        event["entry"][0]["resource"]["parameter"][1]["valueCode"] = json!(notification);
        assert_eq!(
            validate_notification(&event, "Subscription/notify", |_| unreachable!()),
            Ok(())
        );
    }
}

#[test]
fn state_reviewer_rules_depend_on_state_plan_date_and_decision_basis() {
    let rules = ReviewerPolicy::state_rules();
    let reviewer = ReviewerEvidence {
        profession: Profession::Physician,
        licensed_and_in_scope: true,
        individually_reviewed: true,
        independent_judgment: true,
        competent_for_service: true,
        actual_clinical_experience: true,
        board_certified_or_eligible_in_same_specialty: true,
        medical_director: false,
    };
    let context = ReviewContext {
        state: State::TX,
        plan: Plan::StateRegulatedCommercial,
        on: date(2026, 7, 1),
        issued_or_renewed: Some(date(2026, 1, 1)),
        decision: Decision::Denial,
        basis: Basis::MedicalNecessity,
        specialty: Specialty::Medical,
    };
    use Automation::*;
    assert_eq!(
        rules.verify(context, ContributedToDecision, Some(reviewer)),
        Err(ReviewError::ProhibitedAutomation)
    );
    let older_plan = ReviewContext {
        issued_or_renewed: Some(date(2025, 12, 31)),
        ..context
    };
    assert_eq!(
        rules.verify(older_plan, ContributedToDecision, Some(reviewer)),
        Ok(())
    );
    assert_eq!(
        rules.verify(context, NoneOrAdministrativeSupport, Some(reviewer)),
        Ok(())
    );
    assert_eq!(
        rules.verify(
            ReviewContext {
                issued_or_renewed: None,
                ..context
            },
            NoneOrAdministrativeSupport,
            Some(reviewer)
        ),
        Err(ReviewError::MissingPlanDate)
    );

    let wa = ReviewContext {
        state: State::WA,
        plan: Plan::StatePublicEmployees,
        ..context
    };
    assert_eq!(
        rules.verify(wa, ContributedToDecision, None),
        Err(ReviewError::UnqualifiedReviewer)
    );
    assert_eq!(
        rules.verify(
            ReviewContext {
                on: date(2026, 6, 10),
                ..wa
            },
            ContributedToDecision,
            None
        ),
        Ok(())
    );
    assert_eq!(
        rules.verify(
            ReviewContext {
                on: date(2026, 6, 11),
                ..wa
            },
            SoleDecisionMaker,
            Some(reviewer)
        ),
        Err(ReviewError::ProhibitedAutomation)
    );
    assert_eq!(
        rules.verify(
            ReviewContext {
                decision: Decision::Approval,
                ..wa
            },
            SoleDecisionMaker,
            None
        ),
        Ok(())
    );

    let ca = ReviewContext {
        state: State::CA,
        decision: Decision::Delay,
        ..context
    };
    assert_eq!(
        rules.verify(ca, NoneOrAdministrativeSupport, None),
        Err(ReviewError::UnqualifiedReviewer)
    );
    assert_eq!(
        rules.verify(ca, ContributedToDecision, Some(reviewer)),
        Err(ReviewError::ProhibitedAutomation)
    );
    assert_eq!(
        rules.verify(
            ReviewContext {
                decision: Decision::Approval,
                ..ca
            },
            SoleDecisionMaker,
            None
        ),
        Err(ReviewError::ProhibitedAutomation)
    );
    assert_eq!(
        rules.verify(
            ReviewContext {
                basis: Basis::Administrative,
                ..ca
            },
            NoneOrAdministrativeSupport,
            None
        ),
        Ok(())
    );
    assert_eq!(
        rules.verify(
            ReviewContext {
                plan: Plan::SelfFundedErisa,
                ..ca
            },
            NoneOrAdministrativeSupport,
            None
        ),
        Ok(())
    );

    let md = ReviewContext {
        state: State::MD,
        decision: Decision::Modification,
        plan: Plan::MedicaidManagedCare,
        ..context
    };
    let wrong_specialty = ReviewerEvidence {
        board_certified_or_eligible_in_same_specialty: false,
        ..reviewer
    };
    assert_eq!(
        rules.verify(md, ContributedToDecision, Some(wrong_specialty)),
        Err(ReviewError::UnqualifiedReviewer)
    );
    assert_eq!(
        rules.verify(
            ReviewContext {
                specialty: Specialty::MentalHealthOrSubstanceUse,
                ..md
            },
            ContributedToDecision,
            Some(wrong_specialty)
        ),
        Ok(())
    );
    assert_eq!(
        rules.verify(
            ReviewContext {
                specialty: Specialty::Dental,
                ..md
            },
            ContributedToDecision,
            Some(ReviewerEvidence {
                profession: Profession::Dentist,
                ..reviewer
            })
        ),
        Ok(())
    );

    let az = ReviewContext {
        state: State::AZ,
        ..context
    };
    assert_eq!(
        rules.verify(az, NoneOrAdministrativeSupport, Some(reviewer)),
        Err(ReviewError::UnqualifiedReviewer)
    );
    assert_eq!(
        rules.verify(
            az,
            ContributedToDecision,
            Some(ReviewerEvidence {
                medical_director: true,
                ..reviewer
            })
        ),
        Ok(())
    );
    assert_eq!(
        rules.verify(
            ReviewContext {
                on: date(2026, 6, 30),
                ..az
            },
            NoneOrAdministrativeSupport,
            None
        ),
        Ok(())
    );
}

// Synthetic self-insured request in the shape the HL7 PAS 2.2.1 examples use: one Patient that
// declares profile-subscriber is Claim.patient and Coverage.beneficiary, Coverage has no
// Coverage.subscriber, the member id sits in Coverage.subscriberId, and the Coverage.identifier
// MB slice carries only its type. Relationship carries the X12 self code 18 beside the FHIR code.
fn subscriber_request() -> Value {
    serde_json::from_str(include_str!("fixtures/pas/request_subscriber.json")).unwrap()
}

fn resource<'a>(bundle: &'a mut Value, kind: &str) -> &'a mut Value {
    bundle["entry"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .map(|entry| &mut entry["resource"])
        .find(|resource| resource["resourceType"] == kind)
        .unwrap()
}

fn set_patient_profile(bundle: &mut Value, profile: &str) {
    resource(bundle, "Patient")["meta"]["profile"] = json!([format!("{PAS}{profile}")]);
}

fn dependent_patient() -> Value {
    json!({
        "fullUrl":"https://example.test/fhir/Patient/dependent",
        "resource":{
            "resourceType":"Patient","id":"dependent",
            "meta":{"profile":[format!("{PAS}profile-beneficiary")]},
            "identifier":[{"system":"https://example.test/member-ids","value":"synthetic-member-2"}],
            "name":[{"given":["Sample"],"family":"Dependent"}],
            "gender":"unknown"
        }
    })
}

#[test]
fn one_patient_fills_claim_patient_and_coverage_beneficiary_under_either_patient_profile() {
    // Claim.patient and Coverage.beneficiary have targetProfile profile-beneficiary, while the HL7
    // 2.2.1 examples declare profile-subscriber on that Patient. Both profiles derive from US Core
    // Patient 7.0.0 and differ only by optional elements (extension-militaryStatus; the
    // multipleBirthInteger slice), so a Patient declaring either one satisfies the role.
    // https://hl7.org/fhir/us/davinci-pas/2.2.1/StructureDefinition-profile-beneficiary.html
    // https://hl7.org/fhir/us/davinci-pas/2.2.1/StructureDefinition-profile-subscriber.html
    let mut value = subscriber_request();
    let parsed = validate_request(&value).unwrap();
    assert_eq!(parsed.priority(), RequestPriority::Standard);
    assert_eq!(
        parsed.items().map(|item| item.sequence).collect::<Vec<_>>(),
        [1]
    );
    for profile in [
        "profile-beneficiary",
        "profile-beneficiary|2.2.1",
        "profile-subscriber|2.2.1",
    ] {
        set_patient_profile(&mut value, profile);
        assert!(validate_request(&value).is_ok(), "{profile}");
    }

    // A PAS profile for another resource type, or another PAS version, is still rejected.
    for profile in ["profile-insurer", "profile-subscriber|2.1.0"] {
        set_patient_profile(&mut value, profile);
        assert_eq!(
            validate_request(&value).err(),
            Some(Error::Profile("profile-beneficiary")),
            "{profile}"
        );
    }

    // The shared required content is still enforced: US Core Patient identifier 1..* with
    // system 1..1 and value 1..1, name 1..*, gender 1..1.
    set_patient_profile(&mut value, "profile-subscriber");
    let valid = value.clone();
    for field in ["identifier", "name", "gender"] {
        value = valid.clone();
        resource(&mut value, "Patient")
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert_eq!(
            validate_request(&value).err(),
            Some(Error::Profile("profile-beneficiary")),
            "{field}"
        );
    }
    value = valid;
    resource(&mut value, "Patient")["identifier"][0]
        .as_object_mut()
        .unwrap()
        .remove("system");
    assert_eq!(
        validate_request(&value).err(),
        Some(Error::Profile("profile-beneficiary"))
    );
}

#[test]
fn response_patient_may_declare_either_patient_profile() {
    let submission = request();
    let request = validate_request(&submission).unwrap();
    let mut value = response();
    for profile in ["profile-subscriber|2.2.1", "profile-subscriber"] {
        set_patient_profile(&mut value, profile);
        assert!(validate_response(&value, &request, ResponseContext::default()).is_ok());
    }
    set_patient_profile(&mut value, "profile-insurer");
    assert!(matches!(
        validate_response(&value, &request, ResponseContext::default()),
        Err(Error::Profile("profile-beneficiary"))
    ));
}

#[test]
fn coverage_subscriber_is_required_without_the_x12_self_code_and_must_be_a_patient() {
    // profile-coverage invariant self-beneficiary (error): without a coding whose code is 18,
    // Coverage.subscriber SHALL be present. Coverage.subscriber has targetProfile
    // profile-subscriber.
    // https://hl7.org/fhir/us/davinci-pas/2.2.1/StructureDefinition-profile-coverage.html
    let mut value = subscriber_request();
    resource(&mut value, "Coverage")["relationship"]["coding"] = json!([{
        "system":"http://terminology.hl7.org/CodeSystem/subscriber-relationship","code":"self"
    }]);
    assert_eq!(
        validate_request(&value).err(),
        Some(Error::Missing("Coverage.subscriber"))
    );
    resource(&mut value, "Coverage")["subscriber"] = json!({"reference":"Patient/subscriber"});
    assert!(validate_request(&value).is_ok());

    // Dependent: Claim.patient and Coverage.beneficiary are the dependent, the subscriber is a
    // separate profile-subscriber Patient.
    let mut dependent = subscriber_request();
    dependent["entry"]
        .as_array_mut()
        .unwrap()
        .push(dependent_patient());
    resource(&mut dependent, "Claim")["patient"]["reference"] = json!("Patient/dependent");
    let coverage = resource(&mut dependent, "Coverage");
    coverage["beneficiary"]["reference"] = json!("Patient/dependent");
    coverage["relationship"]["coding"] = json!([
        {"system":"http://terminology.hl7.org/CodeSystem/subscriber-relationship","code":"child"},
        {"system":"https://codesystem.x12.org/005010/1069","code":"19"}
    ]);
    assert_eq!(
        validate_request(&dependent).err(),
        Some(Error::Missing("Coverage.subscriber"))
    );
    resource(&mut dependent, "Coverage")["subscriber"] = json!({"reference":"Patient/subscriber"});
    assert!(validate_request(&dependent).is_ok());
    set_patient_profile(&mut dependent, "profile-beneficiary");
    assert!(validate_request(&dependent).is_ok());

    let valid = dependent.clone();
    resource(&mut dependent, "Coverage")["subscriber"]["reference"] = json!("Organization/payer");
    assert_eq!(
        validate_request(&dependent).err(),
        Some(Error::ReferenceType)
    );
    dependent = valid.clone();
    resource(&mut dependent, "Coverage")["subscriber"]["reference"] = json!("Patient/unknown");
    assert_eq!(validate_request(&dependent).err(), Some(Error::Reference));
    dependent = valid.clone();
    set_patient_profile(&mut dependent, "profile-insurer");
    assert_eq!(
        validate_request(&dependent).err(),
        Some(Error::Profile("profile-subscriber"))
    );
    dependent = valid;
    resource(&mut dependent, "Patient")
        .as_object_mut()
        .unwrap()
        .remove("gender");
    assert_eq!(
        validate_request(&dependent).err(),
        Some(Error::Profile("profile-subscriber"))
    );
}

#[test]
fn coverage_member_id_slice_is_matched_by_its_mb_type_alone() {
    // profile-coverage slices Coverage.identifier by the type pattern MB; identifier:memberid is
    // 1..1 and its system and value are 0..1 (the HL7 examples send only the type).
    // https://hl7.org/fhir/us/davinci-pas/2.2.1/StructureDefinition-profile-coverage.html
    let mut value = subscriber_request();
    assert!(validate_request(&value).is_ok());
    let member = resource(&mut value, "Coverage")["identifier"][0].clone();

    let mut other = value.clone();
    resource(&mut other, "Coverage")["identifier"]
        .as_array_mut()
        .unwrap()
        .push(json!({"system":"https://example.test/identifiers","value":"group-1"}));
    assert!(validate_request(&other).is_ok());

    let mut twice = value.clone();
    resource(&mut twice, "Coverage")["identifier"]
        .as_array_mut()
        .unwrap()
        .push(member);
    assert_eq!(
        validate_request(&twice).err(),
        Some(Error::Profile("profile-coverage"))
    );
    resource(&mut value, "Coverage")["identifier"][0]["type"] = json!({"text":"Member number"});
    assert_eq!(
        validate_request(&value).err(),
        Some(Error::Profile("profile-coverage"))
    );
}

#[test]
fn collection_bundles_must_not_carry_total() {
    // FHIR R4 invariant bdl-1: total is only allowed for searchset and history Bundles.
    // https://hl7.org/fhir/R4/bundle.html#invs
    let submission = request();
    assert!(validate_request(&submission).is_ok());
    for total in [json!(0), json!(2)] {
        let mut value = submission.clone();
        value["total"] = total;
        assert!(matches!(validate_request(&value), Err(Error::Bundle)));
    }
    let parsed = validate_request(&submission).unwrap();
    let mut reply = response();
    assert!(validate_response(&reply, &parsed, ResponseContext::default()).is_ok());
    reply["total"] = json!(1);
    assert!(matches!(
        validate_response(&reply, &parsed, ResponseContext::default()),
        Err(Error::Bundle)
    ));
}

#[test]
fn claim_diagnosis_and_procedure_require_their_code() {
    // profile-claim: Claim.diagnosis.diagnosis[x] is 1..1 and restricted to CodeableConcept;
    // Claim.procedure.procedure[x] is 1..1 (CodeableConcept or Reference).
    // https://hl7.org/fhir/us/davinci-pas/2.2.1/StructureDefinition-profile-claim.html
    let code =
        json!({"coding":[{"system":"https://example.test/synthetic-diagnoses","code":"D1"}]});
    let mut value = request();
    value["entry"][0]["resource"]["diagnosis"] =
        json!([{"sequence":1,"diagnosisCodeableConcept":code}]);
    value["entry"][0]["resource"]["procedure"] =
        json!([{"sequence":1,"procedureCodeableConcept":code}]);
    value["entry"][0]["resource"]["item"][0]["diagnosisSequence"] = json!([1]);
    value["entry"][0]["resource"]["item"][0]["procedureSequence"] = json!([1]);
    assert!(validate_request(&value).is_ok());

    let diagnosis = |entries: Value| {
        let mut changed = value.clone();
        changed["entry"][0]["resource"]["diagnosis"] = entries;
        validate_request(&changed).err()
    };
    let missing = Some(Error::Missing("Claim.diagnosis.diagnosis[x]"));
    let invalid = Some(Error::Profile("Claim.diagnosis.diagnosis[x]"));
    assert_eq!(diagnosis(json!([{"sequence":1}])), missing);
    assert_eq!(
        diagnosis(json!([{"sequence":1,"diagnosisCodeableConcept":code},{"sequence":2}])),
        missing
    );
    assert_eq!(
        diagnosis(json!([{"sequence":1,"diagnosisCodeableConcept":{}}])),
        missing
    );
    assert_eq!(
        diagnosis(json!([{
            "sequence":1,
            "diagnosisCodeableConcept":{"coding":[{"system":"https://example.test/synthetic-diagnoses"}]}
        }])),
        missing
    );
    assert_eq!(
        diagnosis(json!([{"sequence":1,"diagnosisReference":{"reference":"Condition/c1"}}])),
        invalid
    );
    assert_eq!(
        diagnosis(json!([{
            "sequence":1,"diagnosisCodeableConcept":code,
            "diagnosisReference":{"reference":"Condition/c1"}
        }])),
        invalid
    );
    assert_eq!(
        diagnosis(json!([{"diagnosisCodeableConcept":code}])),
        Some(Error::ItemCorrelation)
    );

    let procedure = |entries: Value| {
        let mut changed = value.clone();
        changed["entry"][0]["resource"]["procedure"] = entries;
        validate_request(&changed).err()
    };
    let logical = json!({"identifier":{"system":"https://example.test/identifiers","value":"p1"}});
    assert_eq!(
        procedure(json!([{"sequence":1,"procedureReference":logical}])),
        None
    );
    assert_eq!(
        procedure(json!([{"sequence":1}])),
        Some(Error::Missing("Claim.procedure.procedure[x]"))
    );
    assert_eq!(
        procedure(json!([{"sequence":1,"procedureReference":{}}])),
        Some(Error::Missing("Claim.procedure.procedure[x]"))
    );
    assert_eq!(
        procedure(json!([{
            "sequence":1,"procedureCodeableConcept":code,"procedureReference":logical
        }])),
        Some(Error::Profile("Claim.procedure.procedure[x]"))
    );
}

// The four request Bundles published with PAS 2.2.1 (also under example/ in package.tgz):
//   https://hl7.org/fhir/us/davinci-pas/2.2.1/Bundle-HomecareAuthorizationBundleExample.json
//   https://hl7.org/fhir/us/davinci-pas/2.2.1/Bundle-MedicalServicesAuthorizationBundleExample.json
//   https://hl7.org/fhir/us/davinci-pas/2.2.1/Bundle-ReferralAuthorizationBundleExample.json
//   https://hl7.org/fhir/us/davinci-pas/2.2.1/Bundle-SurgicalRequestBundleExample.json
// CI downloads and verifies the HL7 content before running this test explicitly.
#[test]
#[ignore = "requires the four published HL7 PAS 2.2.1 fixtures"]
fn official_pas_examples_follow_the_published_profiles() {
    let dir = std::env::var_os("MEDVERIFY_PAS_EXAMPLES")
        .expect("MEDVERIFY_PAS_EXAMPLES must contain all four published HL7 PAS 2.2.1 fixtures");
    let load = |name: &str| -> Value {
        let path = std::path::Path::new(&dir).join(name);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        parse_json(&text).unwrap()
    };
    let sequences = |value: &Value| {
        validate_request(value).ok().map(|parsed| {
            (
                parsed.items().map(|item| item.sequence).collect::<Vec<_>>(),
                parsed.priority(),
            )
        })
    };

    // Accepted: a profile-subscriber Patient is Claim.patient and Coverage.beneficiary, and the
    // Coverage.identifier MB slice has only its type (system and value are 0..1).
    for (name, items, priority) in [
        (
            "Bundle-HomecareAuthorizationBundleExample.json",
            vec![1, 2],
            RequestPriority::Standard,
        ),
        (
            "Bundle-MedicalServicesAuthorizationBundleExample.json",
            vec![1],
            RequestPriority::Standard,
        ),
        (
            "Bundle-ReferralAuthorizationBundleExample.json",
            vec![1],
            RequestPriority::Expedited,
        ),
    ] {
        let value = load(name);
        assert_eq!(sequences(&value), Some((items, priority)), "{name}");

        let mut changed = value.clone();
        set_patient_profile(&mut changed, "profile-beneficiary");
        assert!(validate_request(&changed).is_ok(), "{name}");
        // PAS profile for another resource type: still rejected.
        set_patient_profile(&mut changed, "profile-insurer");
        assert_eq!(
            validate_request(&changed).err(),
            Some(Error::Profile("profile-beneficiary")),
            "{name}"
        );
        // bdl-1: total on a collection Bundle.
        changed = value.clone();
        changed["total"] = json!(1);
        assert!(
            matches!(validate_request(&changed), Err(Error::Bundle)),
            "{name}"
        );
        // self-beneficiary: the examples are self-insured through the X12 code 18 alone.
        changed = value.clone();
        resource(&mut changed, "Coverage")["relationship"]["coding"]
            .as_array_mut()
            .unwrap()
            .retain(|coding| coding["code"] != "18");
        assert_eq!(
            validate_request(&changed).err(),
            Some(Error::Missing("Coverage.subscriber")),
            "{name}"
        );
        // profile-claim: diagnosis[x] 1..1.
        changed = value.clone();
        resource(&mut changed, "Claim")["diagnosis"] = json!([{"sequence":1}]);
        assert_eq!(
            validate_request(&changed).err(),
            Some(Error::Missing("Claim.diagnosis.diagnosis[x]")),
            "{name}"
        );
    }

    // Rejected: PractitionerRole/SurgicalPractitionerRoleExample points at
    // Practitioner/ReferralPractitionerExample and Location/ReferralLocationExample, neither of
    // which is in the Bundle. PAS 2.2.1 specification.html#spec-13: additional Bundle entries
    // SHALL be populated with any resources referenced by the Claim resource (and any resources
    // referenced by those resources, fully traversing all references).
    let surgical = load("Bundle-SurgicalRequestBundleExample.json");
    assert_eq!(validate_request(&surgical).err(), Some(Error::Reference));
    let mut repaired = surgical.clone();
    resource(&mut repaired, "PractitionerRole")["practitioner"]["reference"] =
        json!("Practitioner/SurgicalPractitionerExample");
    assert_eq!(validate_request(&repaired).err(), Some(Error::Reference));
    resource(&mut repaired, "PractitionerRole")["location"][0]["reference"] =
        json!("Location/SurgicalLocationExample");
    assert_eq!(
        sequences(&repaired),
        Some((vec![1], RequestPriority::Standard))
    );
    let mut stripped = repaired.clone();
    resource(&mut stripped, "Claim")["diagnosis"][0]
        .as_object_mut()
        .unwrap()
        .remove("diagnosisCodeableConcept");
    assert_eq!(
        validate_request(&stripped).err(),
        Some(Error::Missing("Claim.diagnosis.diagnosis[x]"))
    );
}

#[test]
fn assess_item_rejects_an_expedited_request_on_the_standard_clock() {
    let mut value = request();
    value["entry"][0]["resource"]["priority"]["coding"][0]["code"] = json!("stat");
    let expedited = validate_request(&value).unwrap();
    assert_eq!(expedited.priority(), RequestPriority::Expedited);
    let item = expedited.items().find(|item| item.sequence == 1).unwrap();
    assert_eq!(item.priority(), RequestPriority::Expedited);

    // Notice on day four: after 72 hours, inside seven calendar days.
    let received = zoned("2026-07-06T09:00:00-07:00[America/Los_Angeles]");
    let event = Some(CommunicatedAction {
        action: Action::Approved,
        at: ts("2026-07-10T09:00:00-07:00"),
    });
    let plan = Plan::MedicareAdvantage;
    assert_eq!(
        timeline(&received, plan, Priority::Standard).assess_item(item, medical, event),
        Err(TimingError::PriorityDowngrade)
    );
    assert_eq!(
        timeline(&received, plan, Priority::Expedited).assess_item(item, medical, event),
        Ok(Timeliness::Late {
            deadline: ts("2026-07-09T16:00:00Z")
        })
    );

    // A payer may expedite a standard request; the stricter clock applies.
    let value = request();
    let standard = validate_request(&value).unwrap();
    let item = standard.items().find(|item| item.sequence == 1).unwrap();
    assert_eq!(item.priority(), RequestPriority::Standard);
    assert_eq!(
        timeline(&received, plan, Priority::Expedited).assess_item(item, medical, event),
        Ok(Timeliness::Late {
            deadline: ts("2026-07-09T16:00:00Z")
        })
    );
    assert_eq!(
        timeline(&received, plan, Priority::Standard).assess_item(item, medical, event),
        Ok(Timeliness::OnTime {
            deadline: ts("2026-07-13T16:00:00Z")
        })
    );
}

#[test]
fn parse_json_rejects_repeated_properties_at_any_depth() {
    let text = include_str!("fixtures/pas/response.json");
    let repeated = text.replacen(
        "\"itemSequence\": 5,",
        "\"itemSequence\": 1, \"itemSequence\": 5,",
        1,
    );
    assert_ne!(repeated, text);
    // A plain parse keeps the last value, so the repeated line would correlate silently.
    let request = request();
    let request = validate_request(&request).unwrap();
    let merged: Value = serde_json::from_str(&repeated).unwrap();
    assert!(validate_response(&merged, &request, ResponseContext::default()).is_ok());

    let error = parse_json(&repeated).unwrap_err();
    assert!(error.is_data(), "{error}");
    assert_eq!(
        error.to_string().split(" at ").next(),
        Some("duplicate property")
    );
    assert!(error.line() > 1);

    for document in [
        r#"{"a": 1, "a": 1}"#,
        r#"[{"a": {"b": [], "b": []}}]"#,
        r#"{"a": [1, {"c": null, "c": null}]}"#,
    ] {
        assert!(parse_json(document).unwrap_err().is_data(), "{document}");
    }
    assert!(parse_json("{").unwrap_err().is_eof());
    assert!(parse_json(r#"{"a": 1} x"#).unwrap_err().is_syntax());
    for document in [
        include_str!("fixtures/pas/request.json"),
        include_str!("fixtures/pas/response.json"),
        include_str!("fixtures/pas/request_subscriber.json"),
        r#"{"a": [1, -2, 3.5, 1e300, 18446744073709551615, "x", true, null, {}], "b": {"a": 1}}"#,
    ] {
        let plain: Value = serde_json::from_str(document).unwrap();
        assert_eq!(parse_json(document).unwrap(), plain);
    }
}

#[test]
fn reviewer_policy_rejects_noncanonical_state_codes() {
    let rule = |state: &str| {
        format!(
            "[[rule]]\nstate = {state:?}\neffective = \"2025-09-01\"\nplans = [\"state_regulated_commercial\"]\ndecisions = [\"denial\"]\nbases = [\"medical_necessity\"]\nsource = \"synthetic\"\n"
        )
    };
    assert!(ReviewerPolicy::from_toml(&rule("TX")).is_ok());
    for state in ["tx", "TX ", "", "ZZ"] {
        assert!(
            ReviewerPolicy::from_toml(&rule(state)).is_err(),
            "{state:?}"
        );
    }
}

const PROCESS_PRIORITY: &str = "http://terminology.hl7.org/CodeSystem/processpriority";

fn with_priority(priority: Value, level: Option<Value>) -> Value {
    let mut value = request();
    let claim = &mut value["entry"][0]["resource"];
    claim["priority"] = priority;
    if let Some(level) = level {
        claim["extension"] = json!([{
            "url": format!("{PAS}extension-levelOfServiceCode"),
            "valueCodeableConcept": level
        }]);
    }
    value
}

fn level(code: &str) -> Value {
    json!({"coding": [{"system": "https://codesystem.x12.org/005010/1338", "code": code}]})
}

fn hl7(code: &str) -> Value {
    json!({"coding": [{"system": PROCESS_PRIORITY, "code": code}]})
}

fn priority_of(value: &Value) -> Result<RequestPriority, Error> {
    validate_request(value).map(|parsed| parsed.priority())
}

#[test]
fn claim_priority_does_not_depend_on_coding_order() {
    let priority = |codes: &[(&str, &str)]| {
        let coding: Vec<_> = codes
            .iter()
            .map(|(system, code)| json!({"system": system, "code": code}))
            .collect();
        priority_of(&with_priority(json!({ "coding": coding }), None))
    };
    let conflict = Err(Error::Profile("Claim.priority"));
    for (first, second, expected) in [
        ("normal", "stat", conflict),
        ("normal", "deferred", conflict),
        ("stat", "deferred", conflict),
        ("stat", "stat", Ok(RequestPriority::Expedited)),
        ("normal", "normal", Ok(RequestPriority::Standard)),
        ("deferred", "deferred", Ok(RequestPriority::Standard)),
    ] {
        let (first, second) = ((PROCESS_PRIORITY, first), (PROCESS_PRIORITY, second));
        assert_eq!(priority(&[first, second]), expected);
        assert_eq!(priority(&[second, first]), expected);
    }
    // Codings from other systems never set the process priority. Beside `normal` they are
    // content the verifier does not read, so the standard clock is not certified (in any order);
    // beside `stat` the expedited floor holds.
    let other = ("https://example.test/local-priority", "stat");
    let normal = (PROCESS_PRIORITY, "normal");
    let stat = (PROCESS_PRIORITY, "stat");
    assert_eq!(
        priority(&[other, normal]),
        Ok(RequestPriority::Undetermined)
    );
    assert_eq!(
        priority(&[normal, other]),
        Ok(RequestPriority::Undetermined)
    );
    assert_eq!(priority(&[other, stat]), Ok(RequestPriority::Expedited));
    assert_eq!(priority(&[stat, other]), Ok(RequestPriority::Expedited));
}

#[test]
fn claim_priority_accepts_exactly_the_r4_process_priority_codes() {
    let priority = |code: &str| priority_of(&with_priority(hl7(code), None));
    assert_eq!(priority("stat"), Ok(RequestPriority::Expedited));
    assert_eq!(priority("normal"), Ok(RequestPriority::Standard));
    assert_eq!(priority("deferred"), Ok(RequestPriority::Standard));
    for undefined in ["urgent", "asap", "routine", "STAT", " stat"] {
        assert_eq!(
            priority(undefined),
            Err(Error::Profile("Claim.priority")),
            "{undefined:?}"
        );
    }
}

#[test]
fn claim_priority_without_an_hl7_code_is_undetermined_unless_the_level_of_service_says_more() {
    use RequestPriority::*;
    let local =
        json!({"coding": [{"system": "https://example.test/local-priority", "code": "routine"}]});
    let code_only = json!({"coding": [{"code": "routine"}]});
    let display_only = json!({"coding": [{"display": "Routine"}]});
    let jeopardy = json!({"text": "Physician: waiting seven days could seriously jeopardize the enrollee's health"});
    for priority in [&local, &code_only, &display_only, &jeopardy] {
        for (level_code, expected) in [
            (None, Undetermined),
            (Some("E"), Undetermined),
            (Some("U"), Expedited),
            (Some("03"), Emergency),
        ] {
            let value = with_priority(priority.clone(), level_code.map(level));
            assert_eq!(
                priority_of(&value),
                Ok(expected),
                "{priority} {level_code:?}"
            );
        }
    }
    // An HL7 code resolves the priority; urgent and emergency levels still take precedence.
    for (code, level_code, expected) in [
        ("normal", Some("E"), Standard),
        ("deferred", Some("E"), Standard),
        ("normal", Some("U"), Expedited),
        ("stat", Some("E"), Expedited),
        ("stat", Some("U"), Expedited),
        ("normal", Some("03"), Emergency),
        ("stat", Some("03"), Emergency),
    ] {
        let value = with_priority(hl7(code), level_code.map(level));
        assert_eq!(priority_of(&value), Ok(expected), "{code} {level_code:?}");
    }
}

#[test]
fn level_of_service_follows_its_required_x12_binding() {
    let invalid = Err(Error::Profile("extension-levelOfServiceCode"));
    let local = json!({"coding": [{"system": "https://example.test/local-level", "code": "U"}]});
    let mixed = json!({"coding": [
        {"system": "https://codesystem.x12.org/005010/1338", "code": "U"},
        {"system": "https://codesystem.x12.org/005010/1338", "code": "E"}
    ]});
    for value in [
        level("X"),
        level("u"),
        local,
        mixed,
        json!({"text": "Urgent"}),
    ] {
        assert_eq!(
            priority_of(&with_priority(hl7("normal"), Some(value.clone()))),
            invalid,
            "{value}"
        );
    }
    // A translation beside the X12 code is allowed.
    let translated = json!({"coding": [
        {"system": "https://example.test/local-level", "code": "routine"},
        {"system": "https://codesystem.x12.org/005010/1338", "code": "U"}
    ]});
    assert_eq!(
        priority_of(&with_priority(hl7("normal"), Some(translated))),
        Ok(RequestPriority::Expedited)
    );
    let mut coding_value = with_priority(hl7("normal"), None);
    coding_value["entry"][0]["resource"]["extension"] = json!([{
        "url": format!("{PAS}extension-levelOfServiceCode"),
        "valueCoding": {"system": "https://codesystem.x12.org/005010/1338", "code": "U"}
    }]);
    assert_eq!(priority_of(&coding_value), invalid);
}

#[test]
fn claim_priority_is_required_and_must_be_a_codeable_concept() {
    let invalid = Err(Error::Profile("Claim.priority"));
    let mut missing = request();
    missing["entry"][0]["resource"]
        .as_object_mut()
        .unwrap()
        .remove("priority");
    assert_eq!(priority_of(&missing), invalid);
    for priority in [
        json!("stat"),
        json!([{"coding": [{"system": PROCESS_PRIORITY, "code": "stat"}]}]),
        json!({}),
        json!({"id": "only-an-id"}),
        json!({"text": ""}),
        json!({"text": 5}),
        json!({"text": ["Immediate"], "coding": [{"system": PROCESS_PRIORITY, "code": "stat"}]}),
    ] {
        assert_eq!(
            priority_of(&with_priority(priority.clone(), None)),
            invalid,
            "{priority}"
        );
    }
    // Any other R4-valid concept (ele-1) is accepted and left for the caller to determine.
    for priority in [
        json!({"text": " "}),
        json!({"coding": [{"system": PROCESS_PRIORITY}]}),
        json!({"coding": [{"display": " "}]}),
        json!({"coding": [{"system": PROCESS_PRIORITY, "_code": {"extension": [{
            "url": "http://hl7.org/fhir/StructureDefinition/data-absent-reason",
            "valueCode": "unknown"
        }]}}]}),
        json!({"_text": {"extension": [{
            "url": "http://hl7.org/fhir/StructureDefinition/data-absent-reason",
            "valueCode": "unknown"
        }]}}),
    ] {
        assert_eq!(
            priority_of(&with_priority(priority.clone(), None)),
            Ok(RequestPriority::Undetermined),
            "{priority}"
        );
    }
}

#[test]
fn required_bindings_need_a_coded_concept_not_text_alone() {
    let mut category = request();
    resource(&mut category, "Claim")["item"][0]["category"] = json!({"text": "Surgical"});
    assert_eq!(
        validate_request(&category).err(),
        Some(Error::Profile("Claim.item"))
    );
    // Text alone would drop the item out of the deadline rule as unclassified.
    let mut product = request();
    resource(&mut product, "Claim")["item"][0]["productOrService"] = json!({"text": "MRI brain"});
    assert_eq!(
        validate_request(&product).err(),
        Some(Error::Profile("Claim.item"))
    );
    let mut kind = request();
    resource(&mut kind, "Claim")["type"] = json!({"text": "Professional"});
    assert!(validate_request(&kind).is_err());
}

#[test]
fn level_of_service_is_read_only_from_the_claim() {
    let misplaced = Err(Error::Profile("extension-levelOfServiceCode"));
    let emergency = json!({
        "url": format!("{PAS}extension-levelOfServiceCode"),
        "valueCodeableConcept": level("03")
    });
    let mut item = request();
    resource(&mut item, "Claim")["item"][0]["extension"] = json!([emergency.clone()]);
    assert_eq!(priority_of(&item), misplaced);
    let mut coverage = request();
    resource(&mut coverage, "Coverage")["extension"] = json!([emergency.clone()]);
    assert_eq!(priority_of(&coverage), misplaced);
    let mut nested = request();
    resource(&mut nested, "Claim")["extension"] = json!([{
        "url": "https://example.test/wrapper",
        "extension": [emergency]
    }]);
    assert_eq!(priority_of(&nested), misplaced);
}

#[test]
fn malformed_codings_are_rejected_instead_of_read_as_absent() {
    let malformed = Err(Error::Profile("CodeableConcept.coding"));
    // A single Coding object where R4 JSON requires an array must not hide `stat`.
    let stat = json!({"system": PROCESS_PRIORITY, "code": "stat"});
    let unwrapped = with_priority(json!({"text": "Immediate", "coding": stat}), None);
    assert_eq!(priority_of(&unwrapped), malformed);
    let wrapped = with_priority(json!({"text": "Immediate", "coding": [stat]}), None);
    assert_eq!(priority_of(&wrapped), Ok(RequestPriority::Expedited));

    for coding in [
        json!([]),
        json!(["stat"]),
        json!([{}]),
        json!([{"id": "only-an-id"}]),
        json!([{"system": PROCESS_PRIORITY, "code": ""}]),
        json!([{"system": PROCESS_PRIORITY, "code": ["stat"]}]),
        json!([{"system": 5, "code": "stat"}]),
        json!([{"system": PROCESS_PRIORITY, "code": "stat", "display": 1}]),
        json!([{"system": PROCESS_PRIORITY, "code": "stat", "userSelected": "yes"}]),
        json!(null),
    ] {
        let value = with_priority(json!({ "text": "Immediate", "coding": coding }), None);
        assert_eq!(priority_of(&value), malformed, "{coding}");
    }

    // The same rule holds everywhere in the Bundle, including extensions and other resources.
    let urgent = with_priority(
        hl7("normal"),
        Some(json!({"coding": {"system": "https://codesystem.x12.org/005010/1338", "code": "U"}})),
    );
    assert_eq!(priority_of(&urgent), malformed);
    let mut drug = request();
    let product = &mut resource(&mut drug, "Claim")["item"][1]["productOrService"];
    product["coding"] = product["coding"][0].clone();
    product["text"] = json!("Synthetic product");
    assert_eq!(priority_of(&drug), malformed);
    let mut coverage = request();
    resource(&mut coverage, "Coverage")["relationship"]["coding"] = json!({"code": "18"});
    assert_eq!(priority_of(&coverage), malformed);

    let submission = request();
    let parsed = validate_request(&submission).unwrap();
    let mut reply = response();
    resource(&mut reply, "ClaimResponse")["item"][0]["adjudication"][0]["category"]["coding"] = json!({"system": "http://terminology.hl7.org/CodeSystem/adjudication", "code": "submitted"});
    assert_eq!(
        validate_response(&reply, &parsed, ResponseContext::default()).err(),
        Some(Error::Profile("CodeableConcept.coding"))
    );
}

#[test]
fn codings_without_a_system_satisfy_example_and_extensible_bindings_only() {
    // Claim.priority (example binding): accepted, and left for the caller to determine.
    let code_only = with_priority(json!({"coding": [{"code": "routine"}]}), None);
    assert_eq!(priority_of(&code_only), Ok(RequestPriority::Undetermined));
    // Coverage.relationship (extensible binding).
    let mut relationship = request();
    resource(&mut relationship, "Coverage")["relationship"] = json!({"coding": [{"code": "18"}]});
    assert!(validate_request(&relationship).is_ok());
    // Claim.procedure.procedure[x] (example binding).
    let mut procedure = request();
    resource(&mut procedure, "Claim")["procedure"] =
        json!([{"sequence": 1, "procedureCodeableConcept": {"coding": [{"code": "0016070"}]}}]);
    assert!(validate_request(&procedure).is_ok());
    // ClaimResponse.adjudication.category (example binding).
    let submission = request();
    let parsed = validate_request(&submission).unwrap();
    let mut reply = response();
    resource(&mut reply, "ClaimResponse")["item"][0]["adjudication"][0]["category"] =
        json!({"coding": [{"code": "submitted"}]});
    assert!(validate_response(&reply, &parsed, ResponseContext::default()).is_ok());

    // Claim.item.category has a required binding: membership needs the system.
    let mut category = request();
    resource(&mut category, "Claim")["item"][0]["category"] = json!({"coding": [{"code": "3"}]});
    assert_eq!(
        validate_request(&category).err(),
        Some(Error::Profile("Claim.item"))
    );
}

#[test]
fn unresolved_request_priorities_need_the_callers_determination_before_a_timing_verdict() {
    let received = zoned("2026-07-06T09:00:00-07:00[America/Los_Angeles]");
    // Notice on day four: after 72 hours, inside seven calendar days.
    let event = Some(CommunicatedAction {
        action: Action::Approved,
        at: ts("2026-07-10T09:00:00-07:00"),
    });
    let ma = |priority| timeline(&received, Plan::MedicareAdvantage, priority);
    let late = Ok(Timeliness::Late {
        deadline: ts("2026-07-09T16:00:00Z"),
    });
    let on_time = Ok(Timeliness::OnTime {
        deadline: ts("2026-07-13T16:00:00Z"),
    });

    // A physician's jeopardy statement in text alone: no verdict on either clock.
    let text = with_priority(
        json!({"text": "Physician: waiting seven days could seriously jeopardize the enrollee's health"}),
        None,
    );
    // Standard code with the X12 emergency level of service.
    let emergency = with_priority(hl7("normal"), Some(level("03")));
    for (value, refusal) in [
        (&text, TimingError::PriorityUndetermined),
        (&emergency, TimingError::EmergencyLevelOfService),
    ] {
        let parsed = validate_request(value).unwrap();
        let item = parsed.items().find(|item| item.sequence == 1).unwrap();
        for clock in [Priority::Standard, Priority::Expedited] {
            assert_eq!(ma(clock).assess_item(item, medical, event), Err(refusal));
        }
        // Exclusions are still reported: there is no clock to resolve.
        let drug = parsed.items().find(|item| item.sequence == 5).unwrap();
        assert_eq!(
            ma(Priority::Standard).assess_item(drug, medical, event),
            Ok(Timeliness::OutsideScope(TimingScope::DrugExcluded))
        );
        assert_eq!(
            timeline(&received, Plan::OriginalMedicare, Priority::Standard)
                .assess_item(item, medical, event),
            Ok(Timeliness::OutsideScope(TimingScope::PayerExcluded))
        );

        // After an explicit determination the usual rules apply.
        let expedited = item.determine(Priority::Expedited).unwrap();
        assert_eq!(expedited.priority(), RequestPriority::Expedited);
        assert_eq!(
            ma(Priority::Standard).assess_item(expedited, medical, event),
            Err(TimingError::PriorityDowngrade)
        );
        assert_eq!(
            ma(Priority::Expedited).assess_item(expedited, medical, event),
            late
        );
        let standard = item.determine(Priority::Standard).unwrap();
        assert_eq!(
            ma(Priority::Standard).assess_item(standard, medical, event),
            on_time
        );
    }

    // A coded `stat` beside the emergency level cannot be determined standard.
    let stat_emergency = with_priority(hl7("stat"), Some(level("03")));
    let parsed = validate_request(&stat_emergency).unwrap();
    let item = parsed.items().find(|item| item.sequence == 1).unwrap();
    assert_eq!(item.priority(), RequestPriority::Emergency);
    assert_eq!(
        ma(Priority::Standard).assess_item(item, medical, event),
        Err(TimingError::EmergencyLevelOfService)
    );
    assert_eq!(
        item.determine(Priority::Standard).err(),
        Some(TimingError::PriorityDowngrade)
    );
    let expedited = item.determine(Priority::Expedited).unwrap();
    assert_eq!(
        ma(Priority::Standard).assess_item(expedited, medical, event),
        Err(TimingError::PriorityDowngrade)
    );
    assert_eq!(
        ma(Priority::Expedited).assess_item(expedited, medical, event),
        late
    );

    // A coded priority cannot be replaced by a determination.
    for value in [with_priority(hl7("stat"), None), request()] {
        let parsed = validate_request(&value).unwrap();
        let item = parsed.items().find(|item| item.sequence == 1).unwrap();
        for priority in [Priority::Standard, Priority::Expedited] {
            assert_eq!(
                item.determine(priority).err(),
                Some(TimingError::PriorityAlreadyCoded)
            );
        }
    }
}

#[test]
fn priority_guard_applies_only_where_the_deadline_rule_applies() {
    let mut value = request();
    value["entry"][0]["resource"]["priority"]["coding"][0]["code"] = json!("stat");
    let expedited = validate_request(&value).unwrap();
    let item = |sequence| {
        expedited
            .items()
            .find(|item| item.sequence == sequence)
            .unwrap()
    };
    let received = zoned("2026-07-06T09:00:00-07:00[America/Los_Angeles]");
    let event = Some(CommunicatedAction {
        action: Action::Approved,
        at: ts("2026-07-10T09:00:00-07:00"),
    });
    let standard = |plan| timeline(&received, plan, Priority::Standard);

    // Item 5 is an NDC drug, item 1 needs the caller's classification.
    assert_eq!(
        standard(Plan::MedicareAdvantage).assess_item(item(5), medical, event),
        Ok(Timeliness::OutsideScope(TimingScope::DrugExcluded))
    );
    assert_eq!(
        standard(Plan::MedicareAdvantage).assess_item(item(1), |_, _| None, event),
        Ok(Timeliness::OutsideScope(TimingScope::UnclassifiedService))
    );
    assert_eq!(
        standard(Plan::OriginalMedicare).assess_item(item(1), medical, event),
        Ok(Timeliness::OutsideScope(TimingScope::PayerExcluded))
    );
    let early = zoned("2025-12-30T09:00:00-08:00[America/Los_Angeles]");
    assert_eq!(
        timeline(&early, Plan::MedicareAdvantage, Priority::Standard).assess_item(
            item(1),
            medical,
            None
        ),
        Ok(Timeliness::OutsideScope(TimingScope::BeforeEffectiveDate))
    );
    assert_eq!(
        standard(Plan::MedicareAdvantage).assess_item(item(1), medical, event),
        Err(TimingError::PriorityDowngrade)
    );
}

const JEOPARDY: &str = "Treating physician: applying the standard timeframe could seriously jeopardize the enrollee's health";

fn day_four() -> Option<CommunicatedAction> {
    Some(CommunicatedAction {
        action: Action::Approved,
        at: ts("2026-07-10T09:00:00-07:00"),
    })
}

fn with_supporting_info(mut value: Value, entries: Value) -> Value {
    resource(&mut value, "Claim")["supportingInfo"] = entries;
    value
}

fn supporting(value: Value) -> Value {
    let mut entry = json!({"sequence": 1, "category": {"coding": [{
        "system": "http://hl7.org/fhir/us/davinci-pas/CodeSystem/PASTempCodes",
        "code": "freeFormMessage"
    }]}});
    for (key, value) in value.as_object().unwrap() {
        entry[key] = value.clone();
    }
    json!([entry])
}

#[test]
fn a_coded_normal_beside_a_physician_statement_is_not_certified_on_the_standard_clock() {
    let received = zoned("2026-07-06T09:00:00-07:00[America/Los_Angeles]");
    let ma = |priority| timeline(&received, Plan::MedicareAdvantage, priority);
    let late = Ok(Timeliness::Late {
        deadline: ts("2026-07-09T16:00:00Z"),
    });
    let on_time = Ok(Timeliness::OnTime {
        deadline: ts("2026-07-13T16:00:00Z"),
    });

    let mut document = request();
    document["entry"].as_array_mut().unwrap().push(json!({
        "fullUrl": "https://example.test/fhir/DocumentReference/statement",
        "resource": {
            "resourceType": "DocumentReference", "id": "statement", "status": "current",
            "content": [{"attachment": {"contentType": "text/plain", "data": "c3RhdGVtZW50"}}]
        }
    }));
    let normal = hl7("normal");
    let mut texted = normal.clone();
    texted["text"] = json!(JEOPARDY);
    let mut local = normal.clone();
    local["coding"].as_array_mut().unwrap().push(
        json!({"system": "https://example.test/local-priority", "code": "physician-expedite"}),
    );
    let mut extended = normal.clone();
    extended["extension"] = json!([{"url": "https://example.test/note", "valueString": JEOPARDY}]);
    let requests = [
        with_priority(texted.clone(), None),
        with_priority(texted, Some(level("E"))),
        with_priority(local, None),
        with_priority(extended, None),
        with_supporting_info(
            with_priority(normal.clone(), None),
            supporting(json!({"valueString": JEOPARDY})),
        ),
        with_supporting_info(
            with_priority(normal.clone(), None),
            supporting(
                json!({"valueAttachment": {"contentType": "text/plain", "data": "c3RhdGVtZW50"}}),
            ),
        ),
        with_supporting_info(
            document.clone(),
            supporting(json!({"valueReference": {"reference": "DocumentReference/statement"}})),
        ),
        with_supporting_info(
            with_priority(normal.clone(), None),
            supporting(json!({"code": {"text": JEOPARDY}})),
        ),
    ];
    for value in &requests {
        let parsed = validate_request(value).unwrap();
        assert_eq!(parsed.priority(), RequestPriority::Undetermined, "{value}");
        let item = parsed.items().find(|item| item.sequence == 1).unwrap();
        // The original failure: notice on day four certified on time on the standard clock.
        for clock in [Priority::Standard, Priority::Expedited] {
            assert_eq!(
                ma(clock).assess_item(item, medical, day_four()),
                Err(TimingError::PriorityUndetermined)
            );
        }
        // Explicit resolution: expedited is late on day four and cannot use the standard clock.
        let expedited = item.determine(Priority::Expedited).unwrap();
        assert_eq!(
            ma(Priority::Expedited).assess_item(expedited, medical, day_four()),
            late
        );
        assert_eq!(
            ma(Priority::Standard).assess_item(expedited, medical, day_four()),
            Err(TimingError::PriorityDowngrade)
        );
        assert_eq!(
            ma(Priority::Standard).assess_determined_item(
                item,
                Some(Priority::Expedited),
                medical,
                day_four()
            ),
            Err(TimingError::PriorityDowngrade)
        );
        // A caller who has read the content and found no such indication may say so.
        let standard = item.determine(Priority::Standard).unwrap();
        assert_eq!(
            ma(Priority::Standard).assess_item(standard, medical, day_four()),
            on_time
        );
    }

    // Ordinary standard requests keep their verdict: the coded priority alone, a display of the
    // coded priority, and structured supporting information.
    let mut displayed = normal.clone();
    displayed["coding"][0]["display"] = json!("Normal");
    for value in [
        request(),
        with_priority(hl7("deferred"), None),
        with_priority(displayed, Some(level("E"))),
        with_supporting_info(
            request(),
            json!([{"sequence": 1,
                "category": {"coding": [{"system": "http://hl7.org/fhir/us/davinci-pas/CodeSystem/PASTempCodes", "code": "admissionDates"}]},
                "timingPeriod": {"start": "2026-07-20", "end": "2026-07-22"}}]),
        ),
    ] {
        let parsed = validate_request(&value).unwrap();
        assert_eq!(parsed.priority(), RequestPriority::Standard, "{value}");
        let item = parsed.items().find(|item| item.sequence == 1).unwrap();
        assert_eq!(
            ma(Priority::Standard).assess_item(item, medical, day_four()),
            on_time
        );
    }

    // The coded expedited floor holds whatever accompanies it.
    let mut stat = hl7("stat");
    stat["text"] = json!("Routine follow-up");
    for value in [
        with_priority(stat, None),
        with_supporting_info(
            with_priority(normal, Some(level("U"))),
            supporting(json!({"valueString": "Routine follow-up"})),
        ),
    ] {
        let parsed = validate_request(&value).unwrap();
        assert_eq!(parsed.priority(), RequestPriority::Expedited, "{value}");
        let item = parsed.items().find(|item| item.sequence == 1).unwrap();
        assert_eq!(
            ma(Priority::Standard).assess_item(item, medical, day_four()),
            Err(TimingError::PriorityDowngrade)
        );
        assert_eq!(
            ma(Priority::Expedited).assess_item(item, medical, day_four()),
            late
        );
    }
}

#[test]
fn codeable_concepts_need_r4_element_content() {
    let procedure = |concept: Value| {
        let mut value = request();
        resource(&mut value, "Claim")["procedure"] =
            json!([{"sequence": 1, "procedureCodeableConcept": concept}]);
        validate_request(&value).map(|parsed| parsed.priority())
    };
    let missing = Err(Error::Missing("Claim.procedure.procedure[x]"));
    for concept in [
        json!({"extension": []}),
        json!({"extension": [{}]}),
        json!({"extension": [{"url": "https://example.test/ext"}]}),
        json!({"extension": [{"url": "", "valueString": "x"}]}),
        json!({"extension": [{"url": "https://example.test/ext", "valueString": null}]}),
        json!({"extension": [{"url": "https://example.test/ext", "valueString": "x", "valueCode": "y"}]}),
        json!({"extension": [{"url": "https://example.test/ext", "valueString": "x",
            "extension": [{"url": "https://example.test/nested", "valueString": "y"}]}]}),
        json!({"extension": [{"url": "https://example.test/ext", "extension": []}]}),
        json!({"extension": {"url": "https://example.test/ext", "valueString": "x"}}),
        json!({"id": "only-an-id"}),
        json!({"id": "only-an-id", "extension": []}),
        json!({"_text": {}}),
        json!({"_text": {"extension": []}}),
        json!({"unknown": "content"}),
        json!({"text": "Lumbar fusion", "unknown": "content"}),
        json!({}),
    ] {
        assert_eq!(procedure(concept.clone()), missing, "{concept}");
    }
    // Code-only, text-only and extension-only concepts are R4 content; the example binding of
    // Claim.procedure.procedure[x] permits each of them.
    for concept in [
        json!({"coding": [{"code": "0016070"}]}),
        json!({"text": "Lumbar fusion"}),
        json!({"extension": [{"url": "https://example.test/ext", "valueString": "x"}]}),
        json!({"extension": [{"url": "https://example.test/ext",
            "extension": [{"url": "https://example.test/nested", "valueCode": "y"}]}]}),
        json!({"_text": {"extension": [{
            "url": "http://hl7.org/fhir/StructureDefinition/data-absent-reason", "valueCode": "unknown"
        }]}}),
        json!({"id": "p1", "text": "Lumbar fusion"}),
    ] {
        assert_eq!(
            procedure(concept.clone()),
            Ok(RequestPriority::Standard),
            "{concept}"
        );
    }
    // The same content rule holds for other open bindings (Claim.priority) ...
    assert_eq!(
        priority_of(&with_priority(json!({"extension": []}), None)),
        Err(Error::Profile("Claim.priority"))
    );
    assert_eq!(
        priority_of(&with_priority(
            json!({"extension": [{"url": "https://example.test/ext", "valueString": "x"}]}),
            None
        )),
        Ok(RequestPriority::Undetermined)
    );
    // ... while a required binding (Claim.item.category) still needs a code from its value set.
    let mut category = request();
    resource(&mut category, "Claim")["item"][0]["category"] =
        json!({"extension": [{"url": "https://example.test/ext", "valueString": "x"}]});
    assert_eq!(
        validate_request(&category).err(),
        Some(Error::Profile("Claim.item"))
    );
}

#[test]
fn exclusions_take_precedence_over_any_priority_determination() {
    let value = request();
    let parsed = validate_request(&value).unwrap();
    assert_eq!(parsed.priority(), RequestPriority::Standard);
    let item = |sequence| {
        parsed
            .items()
            .find(|item| item.sequence == sequence)
            .unwrap()
    };
    let received = zoned("2026-07-06T09:00:00-07:00[America/Los_Angeles]");
    let early = zoned("2025-12-30T09:00:00-08:00[America/Los_Angeles]");
    let outside = |scope| Ok(Timeliness::OutsideScope(scope));
    for determined in [None, Some(Priority::Standard), Some(Priority::Expedited)] {
        for clock in [Priority::Standard, Priority::Expedited] {
            let ma = timeline(&received, Plan::MedicareAdvantage, clock);
            // Item 5 is the fixture's NDC drug.
            assert_eq!(
                ma.assess_determined_item(item(5), determined, medical, day_four()),
                outside(TimingScope::DrugExcluded)
            );
            assert_eq!(
                ma.assess_determined_item(item(1), determined, |_, _| None, day_four()),
                outside(TimingScope::UnclassifiedService)
            );
            assert_eq!(
                timeline(&received, Plan::OriginalMedicare, clock).assess_determined_item(
                    item(1),
                    determined,
                    medical,
                    day_four()
                ),
                outside(TimingScope::PayerExcluded)
            );
            assert_eq!(
                timeline(&early, Plan::MedicareAdvantage, clock).assess_determined_item(
                    item(1),
                    determined,
                    medical,
                    None
                ),
                outside(TimingScope::BeforeEffectiveDate)
            );
        }
    }
    // Within the rule the determination is still checked: a coded priority is not replaced ...
    let ma = |clock| timeline(&received, Plan::MedicareAdvantage, clock);
    for determined in [Priority::Standard, Priority::Expedited] {
        assert_eq!(
            ma(Priority::Expedited).assess_determined_item(
                item(1),
                Some(determined),
                medical,
                day_four()
            ),
            Err(TimingError::PriorityAlreadyCoded)
        );
    }
    // ... and a coded `stat` cannot be determined standard or assessed on the standard clock.
    let stat_emergency = with_priority(hl7("stat"), Some(level("03")));
    let parsed = validate_request(&stat_emergency).unwrap();
    let urgent = parsed.items().find(|item| item.sequence == 1).unwrap();
    assert_eq!(
        ma(Priority::Standard).assess_determined_item(
            urgent,
            Some(Priority::Standard),
            medical,
            day_four()
        ),
        Err(TimingError::PriorityDowngrade)
    );
    assert_eq!(
        ma(Priority::Standard).assess_determined_item(
            urgent,
            Some(Priority::Expedited),
            medical,
            day_four()
        ),
        Err(TimingError::PriorityDowngrade)
    );
    // The same drug exclusion holds for that request.
    let drug = parsed.items().find(|item| item.sequence == 5).unwrap();
    assert_eq!(
        ma(Priority::Standard).assess_determined_item(
            drug,
            Some(Priority::Standard),
            medical,
            day_four()
        ),
        outside(TimingScope::DrugExcluded)
    );
}

fn level_of_service() -> Value {
    json!({"url": format!("{PAS}extension-levelOfServiceCode"), "valueCodeableConcept": level("U")})
}

#[test]
fn response_and_notification_documents_get_the_request_structure_checks() {
    let submission = request();
    let parsed = validate_request(&submission).unwrap();
    let check = |value: &Value| validate_response(value, &parsed, ResponseContext::default());
    assert_eq!(check(&response()).err(), None);
    let misplaced = Some(Error::Profile("extension-levelOfServiceCode"));
    let mut on_response = response();
    resource(&mut on_response, "ClaimResponse")["extension"] = json!([level_of_service()]);
    assert_eq!(check(&on_response).err(), misplaced);
    let mut on_item = response();
    resource(&mut on_item, "ClaimResponse")["item"][0]["extension"] = json!([level_of_service()]);
    assert_eq!(check(&on_item).err(), misplaced);
    let mut on_patient = response();
    resource(&mut on_patient, "Patient")["extension"] = json!([level_of_service()]);
    assert_eq!(check(&on_patient).err(), misplaced);

    let status = |kind: &str| {
        json!({
            "resourceType":"Bundle","type":"history","timestamp":"2026-07-06T09:07:00-07:00",
            "entry":[{"fullUrl":"https://example.test/fhir/Parameters/status","resource":{
                "resourceType":"Parameters","parameter":[
                    {"name":"topic","valueCanonical":TOPIC},
                    {"name":"type","valueCode":kind},
                    {"name":"subscription","valueReference":{"reference":"Subscription/notify"}}
                ]
            }}]
        })
    };
    let error = json!({"name": "error", "valueCodeableConcept": {"coding": [{
        "system": "http://terminology.hl7.org/CodeSystem/subscription-error", "code": "timeout"
    }]}});
    let notify =
        |value: &Value| validate_notification(value, "Subscription/notify", |_| unreachable!());
    for kind in ["heartbeat", "handshake"] {
        let mut valid = status(kind);
        valid["entry"][0]["resource"]["parameter"]
            .as_array_mut()
            .unwrap()
            .push(error.clone());
        assert_eq!(notify(&valid), Ok(()));
        let mut singleton = valid.clone();
        singleton["entry"][0]["resource"]["parameter"][3]["valueCodeableConcept"]["coding"] =
            error["valueCodeableConcept"]["coding"][0].clone();
        assert_eq!(
            notify(&singleton),
            Err(Error::Profile("CodeableConcept.coding"))
        );
        let mut empty = valid.clone();
        empty["entry"][0]["resource"]["parameter"][3]["valueCodeableConcept"]["coding"] = json!([]);
        assert_eq!(
            notify(&empty),
            Err(Error::Profile("CodeableConcept.coding"))
        );
        let mut los = valid;
        los["entry"][0]["resource"]["extension"] = json!([level_of_service()]);
        assert_eq!(
            notify(&los),
            Err(Error::Profile("extension-levelOfServiceCode"))
        );
    }

    // Event notifications: the outer status and the embedded response are both checked.
    let mut event = status("event-notification");
    let parameters = event["entry"][0]["resource"]["parameter"]
        .as_array_mut()
        .unwrap();
    parameters.push(json!({"name":"notification-event","part":[
        {"name":"event-number","valueString":"1"},
        {"name":"focus","valueReference":{"reference":"https://example.test/fhir/Bundle/response"}}
    ]}));
    event["entry"]
        .as_array_mut()
        .unwrap()
        .push(json!({"fullUrl":"https://example.test/fhir/Bundle/response","resource":response()}));
    let correlate = |payload: &Value| check(payload).map(|_| ());
    assert_eq!(
        validate_notification(&event, "Subscription/notify", correlate),
        Ok(())
    );
    let mut singleton = event.clone();
    singleton["entry"][0]["resource"]["parameter"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name": "error", "valueCodeableConcept": {"coding": error["valueCodeableConcept"]["coding"][0].clone()}}));
    assert_eq!(
        validate_notification(&singleton, "Subscription/notify", correlate),
        Err(Error::Profile("CodeableConcept.coding"))
    );
    event["entry"][1]["resource"] = on_response;
    assert_eq!(
        validate_notification(&event, "Subscription/notify", |_| unreachable!()),
        Err(Error::Profile("extension-levelOfServiceCode"))
    );
}
