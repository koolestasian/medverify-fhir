from typing import Literal, TypedDict

Setting = Literal["practitioner", "hospital", "dme"]
ServiceKind = Literal["other", "evaluation_and_management"]
EvidenceBasis = Literal[
    "anatomic_site",
    "unrelated_postoperative_em",
    "significant_separate_em",
    "separate_hospital_em_encounter",
    "decision_for_major_surgery",
    "staged_or_related_postoperative_procedure",
    "unplanned_related_return_to_operating_room",
    "unrelated_postoperative_procedure",
    "repeat_clinical_laboratory_test",
    "separate_encounter",
    "separate_practitioner",
    "separate_structure",
    "non_overlapping_service",
    "distinct_service",
    "repeat_procedure_same_practitioner",
    "repeat_procedure_other_practitioner",
]


class ServiceDay(TypedDict):
    beneficiary: str
    billing_provider: str
    date: str
    setting: Setting


class ModifierIssue(TypedDict):
    line_id: int
    modifier: str
    kind: Literal[
        "missing_evidence", "wrong_service", "more_specific_modifier_required"
    ]
    required_modifier: str | None


class PtpFinding(TypedDict):
    service_day: ServiceDay
    column_one_line_id: int
    column_two_line_id: int
    outcome: Literal["modifier_prohibited", "supported_modifier_required", "bypassed"]
    modifier_issues: list[ModifierIssue]
    abn_cannot_shift_liability: bool


class MueFinding(TypedDict):
    service_day: ServiceDay
    code: str
    scope: Literal[
        "claim_line",
        "unsupported_split",
        "possible_unsupported_split",
        "absolute_service_day_limit",
        "service_day_clinical_review",
    ]
    line_id: int | None
    claim_id: str | None
    max_units: int
    total_units: int
    abn_cannot_shift_liability: bool


class Findings(TypedDict):
    ptp: list[PtpFinding]
    mue: list[MueFinding]
