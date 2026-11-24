from collections.abc import Sequence
from os import PathLike
from typing import final

from ._types import EvidenceBasis, Findings, ServiceKind, Setting

__all__ = ["ClaimLine", "MedVerifyEngine", "ModifierEvidence", "__version__"]
__version__: str

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
