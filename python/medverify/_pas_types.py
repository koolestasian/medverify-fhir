from typing import Literal, NotRequired, TypedDict

Action = Literal["approved", "denied", "pending", "modified", "cancelled"]
Priority = Literal["standard", "expedited"]
RequestPriority = Literal["standard", "expedited", "emergency", "undetermined"]
ServiceKind = Literal["medical", "drug"]
Inquiry = Literal["manual", "automated_polling"]
Plan = Literal[
    "original_medicare",
    "medicare_advantage",
    "applicable_integrated_plan",
    "medicaid_ffs",
    "medicaid_managed_care",
    "chip_ffs",
    "chip_managed_care",
    "federal_exchange_qhp",
    "state_regulated_commercial",
    "state_public_employees",
    "self_funded_erisa",
    "other",
]


class Payer(TypedDict):
    plan: Plan
    rating_period_start: NotRequired[str | None]


class ExtensionNotice(TypedDict):
    sent_at: str
    written_reason: bool
    grievance_rights: NotRequired[Priority | None]


class Extension(TypedDict):
    calendar_days: int
    reason: Literal[
        "enrollee_requested",
        "provider_requested",
        "medicaid_agency_needs_provider_information",
        "chip_physician_or_plan_needs_information",
        "managed_care_needs_information_in_enrollee_interest",
        "integrated_plan_information_likely_to_lead_to_approval_in_enrollee_interest",
        "ma_noncontract_evidence_may_change_denial_in_enrollee_interest",
        "ma_extraordinary_circumstances_in_enrollee_interest",
    ]
    granted_at: str
    notice: NotRequired[ExtensionNotice | None]


class TimelineInput(TypedDict):
    payer: Payer
    received: str
    priority: Priority
    checked_at: str
    extension: NotRequired[Extension | None]
    earlier_deadline: NotRequired[str | None]


class CommunicatedAction(TypedDict):
    action: Action
    at: str


class _OutsideScope(TypedDict):
    outside_scope: Literal[
        "drug_excluded",
        "payer_excluded",
        "before_effective_date",
        "unclassified_service",
    ]


class _Deadline(TypedDict):
    deadline: str


class _OnTime(TypedDict):
    on_time: _Deadline


class _Late(TypedDict):
    late: _Deadline


class _AwaitingDecision(TypedDict):
    awaiting_decision: _Deadline


class _Overdue(TypedDict):
    overdue: _Deadline


Timeliness = _OutsideScope | _OnTime | _Late | _AwaitingDecision | _Overdue

_Decision = Literal["approval", "denial", "modification", "delay"]
_Basis = Literal["medical_necessity", "clinical_judgment", "administrative"]
Automation = Literal[
    "none_or_administrative_support",
    "contributed_to_decision",
    "sole_decision_maker",
]


class ReviewContext(TypedDict):
    state: str
    plan: Plan
    on: str
    issued_or_renewed: NotRequired[str | None]
    decision: _Decision
    basis: _Basis
    specialty: Literal["medical", "mental_health_or_substance_use", "dental"]


class ReviewerEvidence(TypedDict):
    profession: Literal["physician", "dentist", "other_clinician"]
    licensed_and_in_scope: bool
    individually_reviewed: bool
    independent_judgment: bool
    competent_for_service: bool
    actual_clinical_experience: bool
    board_certified_or_eligible_in_same_specialty: bool
    medical_director: bool


class ReviewerRule(TypedDict):
    state: str
    effective: str
    issued_or_renewed_from: str | None
    plans: list[Plan]
    decisions: list[_Decision]
    bases: list[_Basis]
    qualification: (
        Literal[
            "licensed_competent_clinician",
            "maryland_qualified_reviewer",
            "medical_director",
        ]
        | None
    )
    prohibit_automated_contribution: bool
    prohibit_automated_only: bool
    source: str
