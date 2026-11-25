from collections.abc import Callable, Sequence
from collections.abc import Set as AbstractSet
from os import PathLike
from typing import final

from . import _pas_types as pas
from . import _wiser_types as wi
from ._types import EvidenceBasis, Findings, ServiceKind, Setting

__all__ = [
    "ClaimLine",
    "MedVerifyEngine",
    "ModifierEvidence",
    "Policy",
    "Response",
    "ReviewerPolicy",
    "Timeline",
    "ValidatedRequest",
    "__version__",
    "_demo_documents",
    "_demo_engine",
    "evaluate_esi",
    "evaluate_fusion",
    "validate_inquiry",
    "validate_notification",
    "validate_request",
    "validate_response",
    "validate_subscription",
    "validate_utn",
]
__version__: str

def _demo_engine(index_dir: str | PathLike[str]) -> MedVerifyEngine: ...
def _demo_documents() -> tuple[str, str]: ...

@final
class ModifierEvidence:
    def __new__(
        cls, related_line_id: int, modifier: str, basis: EvidenceBasis
    ) -> ModifierEvidence: ...

@final
class ClaimLine:
    def __new__(
        cls,
        *,
        id: int,
        beneficiary: str,
        billing_provider: str,
        date: str,
        setting: Setting,
        code: str,
        units: int,
        service_kind: ServiceKind = "other",
        modifiers: Sequence[str] | None = None,
        modifier_evidence: Sequence[ModifierEvidence] | None = None,
        abn_liability_shift_attempted: bool = False,
        claim_id: str | None = None,
    ) -> ClaimLine: ...

@final
class MedVerifyEngine:
    def __new__(cls, index_dir: str | PathLike[str]) -> MedVerifyEngine: ...
    def evaluate(self, lines: Sequence[ClaimLine]) -> Findings: ...
    def evaluate_batch(
        self, claims: Sequence[Sequence[ClaimLine]]
    ) -> list[Findings]: ...

@final
class Policy:
    @staticmethod
    def washington_synthetic() -> Policy: ...
    @staticmethod
    def from_toml(text: str) -> Policy: ...
    def route(self, service: wi.Service, stage: wi.Stage) -> wi.Route: ...
    def gold_card_rule(self, as_of: str) -> wi.GoldCardRule | None: ...
    def gold_card_eligible(
        self,
        individual_npi: str,
        period: wi.AssessmentPeriod,
        requests: list[wi.PriorRequest] | tuple[wi.PriorRequest, ...],
    ) -> bool: ...
    def associated_denials(
        self,
        lines: list[wi.AdjudicatedLine] | tuple[wi.AdjudicatedLine, ...],
    ) -> list[int]: ...

def evaluate_esi(evidence: wi.EsiEvidence) -> list[wi.EsiFinding]: ...
def evaluate_fusion(evidence: wi.FusionEvidence) -> list[wi.FusionFinding]: ...
def validate_utn(
    utn: str,
    decision: str,
    service: str,
    site: wi.Site,
    field: wi.UtnField,
) -> wi.UtnError | None: ...

@final
class ValidatedRequest:
    @property
    def sequences(self) -> list[int]: ...
    @property
    def priority(self) -> pas.RequestPriority: ...

@final
class Response:
    def action(self, sequence: int) -> pas.Action | None: ...
    @property
    def business_error(self) -> bool: ...
    @property
    def human_reviewed(self) -> bool | None: ...

def validate_request(document: str) -> ValidatedRequest: ...
def validate_response(
    document: str,
    request: ValidatedRequest,
    *,
    additional_information_requested: Sequence[int] | None = None,
    decision_reason_codes: AbstractSet[str] | None = None,
) -> Response: ...
def validate_subscription(document: str, sending_systems: Sequence[str]) -> None: ...
def validate_inquiry(inquiry: pas.Inquiry) -> None: ...
def validate_notification(
    document: str,
    subscription_reference: str,
    validate_focus: Callable[[str], None] | None = None,
) -> None: ...

@final
class Timeline:
    def __new__(cls, data: pas.TimelineInput) -> Timeline: ...
    def assess(
        self,
        kind: pas.ServiceKind | None,
        event: pas.CommunicatedAction | None = None,
    ) -> pas.Timeliness: ...
    def assess_item(
        self,
        request: ValidatedRequest,
        sequence: int,
        *,
        classifications: list[tuple[str, str, pas.ServiceKind]]
        | tuple[tuple[str, str, pas.ServiceKind], ...]
        | None = None,
        event: pas.CommunicatedAction | None = None,
        determined_priority: pas.Priority | None = None,
    ) -> pas.Timeliness: ...

@final
class ReviewerPolicy:
    @staticmethod
    def state_rules() -> ReviewerPolicy: ...
    @staticmethod
    def from_toml(text: str) -> ReviewerPolicy: ...
    def applicable_rules(
        self, context: pas.ReviewContext
    ) -> list[pas.ReviewerRule]: ...
    def verify(
        self,
        context: pas.ReviewContext,
        automation: pas.Automation,
        reviewer: pas.ReviewerEvidence | None = None,
    ) -> None: ...
