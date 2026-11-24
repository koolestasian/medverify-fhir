import json
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from threading import Barrier

import pytest
from medverify import fhir_pas, wiser

FIXTURES = (
    Path(__file__).resolve().parents[1] / "crates/medverify-core/tests/fixtures/pas"
)
PAS = "http://hl7.org/fhir/us/davinci-pas/StructureDefinition/"
BACKPORT = "http://hl7.org/fhir/uv/subscriptions-backport/StructureDefinition/"
TOPIC = "http://hl7.org/fhir/us/davinci-pas/SubscriptionTopic/PASSubscriptionTopic"


def test_wiser_routing_gold_card_and_associated_denials_match_rust():
    policy = wiser.Policy.washington_synthetic()
    service = {
        "state": "WA",
        "plan": "original_medicare",
        "site": "office",
        "date": "2026-07-06",
        "code": "Y9010",
        "individual_npi": "1000000001",
    }
    category = "epidural_steroid_injection"
    assert policy.route(service, {"claim": None}) == {
        "kind": "pre_payment_review",
        "category": category,
        "decision": None,
    }
    assert policy.route(service, {"claim": "non_affirmation"}) == {
        "kind": "prior_authorization_decision",
        "category": category,
        "decision": "non_affirmation",
    }
    stage = {"planning": {"as_of": "2026-01-05", "elect_prior_authorization": True}}
    assert policy.route(service, stage)["kind"] == "voluntary_prior_authorization"
    assert policy.route(service | {"emergency": True}, stage)["kind"] == "out_of_scope"
    notice = {
        "npi": "1000000001",
        "issuer": "Virtix Health",
        "notice_id": "synthetic-1",
        "effective": "2026-07-06",
        "through": "2027-07-05",
    }
    assert policy.route(service | {"exemption": notice}, stage)["kind"] == "exempt"

    assessment = {
        "from": "2026-01-05",
        "through": "2026-07-31",
        "eligibility_date": "2026-10-01",
    }
    requests = [
        {
            "npi": "1000000001",
            "request_id": f'synthetic "{index}"',
            "submitted": "2026-07-10",
            "revision": 1,
            "outcome": "affirmed" if index < 8 else "non_affirmed",
        }
        for index in range(10)
    ]
    later = assessment | {"eligibility_date": "2027-01-01"}
    assert policy.gold_card_rule("2025-12-31") is None
    assert policy.gold_card_rule("2026-10-01")["percent"] == 80
    assert policy.gold_card_rule("2027-01-01") == {
        "effective": "2027-01-01",
        "percent": 90,
        "minimum_requests": 10,
    }
    assert not policy.gold_card_eligible("1000000001", assessment, requests[:9])
    assert policy.gold_card_eligible("1000000001", assessment, requests)
    assert not policy.gold_card_eligible("1000000001", later, requests)
    requests.append(requests[-1] | {"revision": 2, "outcome": "affirmed"})
    assert policy.gold_card_eligible("1000000001", later, requests)

    lines = (
        {"sequence": 1, "code": "Y9010", "wiser_denied": True},
        {"sequence": 2, "code": "Y9016", "wiser_denied": False},
        {"sequence": 3, "code": "Y9017", "wiser_denied": False},
    )
    assert policy.associated_denials(lines) == [2]
    assert policy.associated_denials(lines[1:]) == []
    with pytest.raises(ValueError, match="WISeR service"):
        policy.route(service | {"site": "invalid"}, stage)
    with pytest.raises(ValueError, match="YYYY-MM-DD"):
        policy.route(service | {"date": "2026-02-30"}, stage)
    with pytest.raises(ValueError, match="WISeR policy TOML"):
        wiser.Policy.from_toml("[invalid")


def test_wiser_esi_fusion_and_utn_match_rust():
    esi = {
        "date": "2026-07-06",
        "region": "lumbar_sacral",
        "approach": "transforaminal",
        "indication": "radicular_symptoms",
        "supporting_exam_and_imaging": True,
        "functional_impact_and_baseline": True,
        "pain_weeks": 4,
        "conservative_care": {"failed_after_weeks": 4},
        "active_rehabilitation": True,
        "guidance": "ct_or_fluoroscopy_with_contrast",
        "separately_billed_guidance": False,
        "bilateral_indicated": False,
        "billing": "professional",
        "lines": [
            {"region": "lumbar_sacral", "level": 1, "laterality": "none", "units": 1}
        ],
        "history": [],
    }
    assert wiser.evaluate_esi(esi) == []
    assert wiser.evaluate_esi(esi | {"guidance": "other"}) == ["imaging"]
    with pytest.raises(ValueError, match="ESI evidence"):
        wiser.evaluate_esi(esi | {"pain_weeks": -1})

    fusion = {
        "indication": "nerve_root_impingement",
        "arm_pain_vas": 4,
        "arm_pain_weeks": 12,
        "failed_multimodal_care": True,
        "limits_adls": True,
        "other_causes_excluded": True,
        "isolated_axial_pain": False,
        "mri_or_ct_level_matches": True,
        "stenosis": "foraminal",
        "imaging_cause": "degenerative_disc_disease",
        "instability": {"translation_tenths_mm": 0, "angulation_tenths_deg": 0},
        "neurology": {
            "myelopathy_class": 0,
            "progressive_deficit_during_care": False,
            "progressive_motor_weakness": False,
            "motor_weakness_limits_adls": False,
            "cord_related_sphincter_loss": False,
            "gait_dexterity_or_sphincter_symptoms": False,
        },
    }
    assert wiser.evaluate_fusion(fusion) == []
    assert wiser.evaluate_fusion(fusion | {"arm_pain_weeks": 11}) == [
        "conservative_care"
    ]

    utn = "AB120000000004"
    field = "manual_institutional_treatment_authorization"

    def check(service, value):
        return wiser.validate_utn(
            utn, "2026-01-05", service, "hospital_outpatient", {field: value}
        )

    assert check("2026-05-04", " " * 18 + utn) is None
    assert check("2026-05-05", " " * 18 + utn) == "outside_validity"
    assert check("2026-05-04", utn + " " * 18) == "wrong_placement"
    with pytest.raises(ValueError, match="fourteen ASCII"):
        wiser.validate_utn("short", "2026-01-05", "2026-05-04", "office", {})


def test_pas_owns_its_request_and_correlates_responses():
    submission = (FIXTURES / "request.json").read_text(encoding="utf-8")
    request = fhir_pas.validate_request(submission)
    del submission
    assert request.sequences == [1, 5]
    assert request.priority == "standard"
    document = (FIXTURES / "response.json").read_text(encoding="utf-8")
    response = fhir_pas.validate_response(document, request)
    assert [response.action(index) for index in (1, 5, 2)] == [
        "approved",
        "approved",
        None,
    ]
    assert not response.business_error
    assert response.human_reviewed is None
    bad = json.loads(document)
    bad["entry"][0]["resource"]["item"][0]["itemSequence"] = 999
    with pytest.raises(ValueError, match="^ItemCorrelation$"):
        fhir_pas.validate_response(json.dumps(bad), request)
    with pytest.raises(ValueError, match="invalid FHIR JSON"):
        fhir_pas.validate_request("{")
    with pytest.raises(ValueError, match="profile-pas-request-bundle"):
        fhir_pas.validate_request("null")
    with pytest.raises(TypeError):
        fhir_pas.validate_response(document, None)


@pytest.mark.parametrize(
    ("received", "priority", "boundary", "deadline"),
    [
        (
            "2026-03-06T09:00:00-08:00[America/Los_Angeles]",
            "expedited",
            "2026-03-09T10:00:00-07:00",
            "2026-03-09T17:00:00Z",
        ),
        (
            "2026-03-06T09:00:00-08:00[America/Los_Angeles]",
            "standard",
            "2026-03-13T09:00:00-07:00",
            "2026-03-13T16:00:00Z",
        ),
        (
            "2026-10-30T09:00:00-07:00[America/Los_Angeles]",
            "expedited",
            "2026-11-02T08:00:00-08:00",
            "2026-11-02T16:00:00Z",
        ),
        (
            "2026-10-30T09:00:00-07:00[America/Los_Angeles]",
            "standard",
            "2026-11-06T09:00:00-08:00",
            "2026-11-06T17:00:00Z",
        ),
    ],
)
def test_pas_timeline_uses_rust_dst_deadlines(received, priority, boundary, deadline):
    timeline = fhir_pas.Timeline(
        {
            "payer": {"plan": "medicare_advantage"},
            "received": received,
            "priority": priority,
            "checked_at": "2026-12-01T00:00:00Z",
        }
    )
    event = {"action": "approved", "at": boundary}
    assert timeline.assess("medical", event) == {"on_time": {"deadline": deadline}}
    just_late = boundary[:19] + ".000000001" + boundary[19:]
    assert timeline.assess("medical", event | {"at": just_late}) == {
        "late": {"deadline": deadline}
    }
    with pytest.raises(ValueError, match="EventOrder"):
        timeline.assess("medical", event | {"at": "2026-01-01T00:00:00Z"})

    request = fhir_pas.validate_request(
        (FIXTURES / "request.json").read_text(encoding="utf-8")
    )
    assert timeline.assess_item(request, 1) == {"outside_scope": "unclassified_service"}
    assert timeline.assess_item(request, 5) == {"outside_scope": "drug_excluded"}
    assert timeline.assess_item(
        request,
        1,
        classifications=[
            ("https://example.test/synthetic-procedures", "Y9010", "medical")
        ],
        event=event,
    ) == {"on_time": {"deadline": deadline}}
    with pytest.raises(ValueError, match="unknown request item sequence"):
        timeline.assess_item(request, 999)


def test_pas_reviewer_policy_preserves_dated_decisions():
    policy = fhir_pas.ReviewerPolicy.state_rules()
    context = {
        "state": "TX",
        "plan": "state_regulated_commercial",
        "on": "2026-07-01",
        "decision": "denial",
        "basis": "medical_necessity",
        "specialty": "medical",
    }
    with pytest.raises(ValueError, match="MissingPlanDate"):
        policy.applicable_rules(context)
    earlier = context | {"issued_or_renewed": "2025-12-31"}
    renewed = context | {"issued_or_renewed": "2026-01-01"}
    reviewer = {
        "profession": "physician",
        "licensed_and_in_scope": True,
        "individually_reviewed": True,
        "independent_judgment": True,
        "competent_for_service": True,
        "actual_clinical_experience": True,
        "board_certified_or_eligible_in_same_specialty": True,
        "medical_director": False,
    }
    assert len(policy.applicable_rules(earlier)) == 1
    rules = policy.applicable_rules(renewed)
    assert len(rules) == 2
    assert rules[0]["qualification"] == "licensed_competent_clinician"
    assert rules[1]["effective"] == "2025-09-01"
    assert rules[1]["prohibit_automated_contribution"]
    policy.verify(earlier, "contributed_to_decision", reviewer)
    policy.verify(renewed, "none_or_administrative_support", reviewer)
    with pytest.raises(ValueError, match="UnqualifiedReviewer"):
        policy.verify(renewed, "none_or_administrative_support")
    with pytest.raises(ValueError, match="ProhibitedAutomation"):
        policy.verify(renewed, "contributed_to_decision", reviewer)
    with pytest.raises(ValueError, match="reviewer policy TOML"):
        fhir_pas.ReviewerPolicy.from_toml("[invalid")


def test_pas_subscription_inquiry_and_notification_match_rust():
    subscription = {
        "resourceType": "Subscription",
        "meta": {"profile": [PAS + "profile-subscription"]},
        "status": "requested",
        "reason": "Receive changes to prior authorizations.",
        "criteria": TOPIC,
        "_criteria": {
            "extension": [
                {
                    "url": BACKPORT + "backport-filter-criteria",
                    "valueString": "org-identifier=sample-clinic",
                }
            ]
        },
        "channel": {
            "type": "rest-hook",
            "endpoint": "https://example.test/callback",
            "payload": "application/fhir+json",
            "_payload": {
                "extension": [
                    {
                        "url": BACKPORT + "backport-payload-content",
                        "valueCode": "full-resource",
                    }
                ]
            },
        },
    }
    fhir_pas.validate_subscription(json.dumps(subscription), ["sample-clinic"])
    with pytest.raises(ValueError, match="Subscription"):
        fhir_pas.validate_subscription(json.dumps(subscription), ["other-clinic"])
    fhir_pas.validate_inquiry("manual")
    with pytest.raises(ValueError, match="Subscription"):
        fhir_pas.validate_inquiry("automated_polling")

    reference = "Subscription/notify"
    url = "https://example.test/fhir/Bundle/response"
    parameters = [
        {"name": "topic", "valueCanonical": TOPIC},
        {"name": "subscription", "valueReference": {"reference": reference}},
        {"name": "type", "valueCode": "event-notification"},
        {
            "name": "notification-event",
            "part": [
                {"name": "event-number", "valueString": "1"},
                {"name": "focus", "valueReference": {"reference": url}},
            ],
        },
    ]
    document = {
        "resourceType": "Bundle",
        "type": "history",
        "timestamp": "2026-07-06T09:07:00-07:00",
        "entry": [
            {
                "fullUrl": "https://example.test/fhir/Parameters/status",
                "resource": {"resourceType": "Parameters", "parameter": parameters},
            },
            {
                "fullUrl": url,
                "resource": json.loads(
                    (FIXTURES / "response.json").read_text(encoding="utf-8")
                ),
            },
        ],
    }
    request = fhir_pas.validate_request(
        (FIXTURES / "request.json").read_text(encoding="utf-8")
    )

    def correlate(focus):
        assert fhir_pas.validate_response(focus, request).action(1) == "approved"

    fhir_pas.validate_notification(json.dumps(document), reference, correlate)
    with pytest.raises(ValueError, match="require validate_focus"):
        fhir_pas.validate_notification(json.dumps(document), reference)
    with pytest.raises(ValueError, match="Notification"):
        fhir_pas.validate_notification(
            json.dumps(document), "Subscription/different", correlate
        )
    document["entry"][1]["resource"]["entry"][0]["resource"]["item"][0][
        "itemSequence"
    ] = 999
    with pytest.raises(ValueError, match="^ItemCorrelation$"):
        fhir_pas.validate_notification(json.dumps(document), reference, correlate)
    document["entry"].pop()
    parameters.pop()
    parameters[-1]["valueCode"] = "heartbeat"
    fhir_pas.validate_notification(json.dumps(document), reference)


def test_policy_bindings_are_safe_to_share_between_threads():
    request = fhir_pas.validate_request(
        (FIXTURES / "request.json").read_text(encoding="utf-8")
    )
    response = (FIXTURES / "response.json").read_text(encoding="utf-8")
    policy = wiser.Policy.washington_synthetic()
    threads = 8
    start = Barrier(threads)

    def assess(_):
        start.wait(timeout=10)
        for _ in range(20):
            assert fhir_pas.validate_response(response, request).action(5) == "approved"
            assert policy.gold_card_rule("2027-01-01")["percent"] == 90

    with ThreadPoolExecutor(max_workers=threads) as pool:
        list(pool.map(assess, range(threads)))


def test_policy_import_preserves_gil_state():
    subprocess.run(
        [
            sys.executable,
            "-I",
            "-Werror",
            "-c",
            """
import sys
gil_enabled = getattr(sys, "_is_gil_enabled", lambda: True)
before = gil_enabled()
from medverify import fhir_pas, wiser
assert gil_enabled() == before
""",
        ],
        check=True,
    )
