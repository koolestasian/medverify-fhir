from typing import Literal, NotRequired, TypedDict

Category = Literal[
    "knee_arthroscopy",
    "induced_nerve_lesions",
    "vagus_nerve_stimulation",
    "phrenic_nerve_stimulation",
    "electrical_nerve_stimulation",
    "incontinence_devices",
    "sacral_nerve_stimulation",
    "impotence_treatment",
    "vertebral_augmentation",
    "epidural_steroid_injection",
    "cervical_fusion",
    "hypoglossal_nerve_stimulation",
    "skin_substitutes",
    "deep_brain_stimulation",
    "percutaneous_lumbar_decompression",
]
Site = Literal["hospital_outpatient", "asc", "office", "home", "inpatient", "other"]
_PriorDecision = Literal["provisional_affirmation", "non_affirmation"]


class GoldCardRule(TypedDict):
    effective: str
    percent: int
    minimum_requests: int


AssessmentPeriod = TypedDict(
    "AssessmentPeriod", {"from": str, "through": str, "eligibility_date": str}
)


class PriorRequest(TypedDict):
    npi: str
    request_id: str
    submitted: str
    revision: int
    outcome: Literal["affirmed", "non_affirmed", "dismissed"]


class Exemption(TypedDict):
    npi: str
    issuer: str
    notice_id: str
    effective: str
    through: str


class Service(TypedDict):
    state: str
    plan: Literal["original_medicare", "railroad_medicare", "other"]
    site: Site
    date: str
    code: str
    emergency: NotRequired[bool]
    inpatient_only: NotRequired[bool]
    subject_to_other_cms_prior_authorization: NotRequired[bool]
    provider_under_upic_review: NotRequired[bool]
    facility_claim: NotRequired[bool]
    diagnoses: NotRequired[list[str] | tuple[str, ...]]
    same_claim_codes: NotRequired[list[str] | tuple[str, ...]]
    individual_npi: NotRequired[str | None]
    exemption: NotRequired[Exemption | None]


class _PlanningDetail(TypedDict):
    as_of: str
    elect_prior_authorization: bool


class _Planning(TypedDict):
    planning: _PlanningDetail


class _Claim(TypedDict):
    claim: _PriorDecision | None


Stage = _Planning | _Claim


class Route(TypedDict):
    kind: Literal[
        "out_of_scope",
        "inactive",
        "delayed",
        "exempt",
        "voluntary_prior_authorization",
        "pre_payment_review",
        "prior_authorization_decision",
    ]
    category: Category | None
    decision: _PriorDecision | None


class AdjudicatedLine(TypedDict):
    sequence: int
    code: str
    wiser_denied: bool


class _Ref(TypedDict):
    loop_id: int
    qualifier: str
    ref02: str


class _Electronic837(TypedDict):
    electronic837: _Ref


class _AscFacility837(TypedDict):
    asc_facility837: _Ref


class _PaperCms1500Item23(TypedDict):
    paper_cms1500_item23: str


class _AscFacilityCms1500Item23(TypedDict):
    asc_facility_cms1500_item23: str


class _ElectronicInstitutional(TypedDict):
    electronic_institutional_treatment_authorization: str


class _ManualInstitutional(TypedDict):
    manual_institutional_treatment_authorization: str


UtnField = (
    _Electronic837
    | _AscFacility837
    | _PaperCms1500Item23
    | _AscFacilityCms1500Item23
    | _ElectronicInstitutional
    | _ManualInstitutional
)
UtnError = Literal["outside_validity", "wrong_claim", "wrong_placement", "wrong_utn"]
_Region = Literal["cervical_thoracic", "lumbar_sacral"]


class _FailedAfterWeeks(TypedDict):
    failed_after_weeks: int


class _NoContrastDetail(TypedDict):
    documented_contraindication: bool


class _NoContrast(TypedDict):
    ct_or_fluoroscopy_without_contrast: _NoContrastDetail


class _UltrasoundDetail(TypedDict):
    documented_allergy_or_pregnancy: bool


class _Ultrasound(TypedDict):
    ultrasound_without_contrast: _UltrasoundDetail


class _SustainedImprovementDetail(TypedDict):
    percent: int
    months: int
    same_scale: bool


class _PoorInitialResponseDetail(TypedDict):
    initial_date: str
    changed_approach_level_or_drug: bool
    documented_rationale: bool


class _SustainedImprovement(TypedDict):
    sustained_improvement: _SustainedImprovementDetail


class _PoorInitialResponse(TypedDict):
    poor_initial_response: _PoorInitialResponseDetail


class EsiLine(TypedDict):
    region: _Region
    level: int
    laterality: Literal["none", "right", "left", "bilateral50"]
    units: int


class EsiSession(TypedDict):
    date: str
    region: _Region


class EsiEvidence(TypedDict):
    date: str
    region: _Region
    approach: Literal["transforaminal", "interlaminar", "caudal"]
    indication: Literal[
        "radicular_symptoms",
        "post_laminectomy_syndrome",
        "acute_herpes_zoster",
        "other",
    ]
    supporting_exam_and_imaging: bool
    functional_impact_and_baseline: bool
    pain_weeks: int
    conservative_care: (
        _FailedAfterWeeks | Literal["intolerant", "refractory_zoster", "undocumented"]
    )
    active_rehabilitation: bool
    guidance: (
        Literal["ct_or_fluoroscopy_with_contrast", "other"] | _NoContrast | _Ultrasound
    )
    separately_billed_guidance: bool
    bilateral_indicated: bool
    billing: Literal["professional", "asc_facility", "hospital_outpatient_facility"]
    lines: list[EsiLine] | tuple[EsiLine, ...]
    history: list[EsiSession] | tuple[EsiSession, ...]
    repeat: NotRequired[_SustainedImprovement | _PoorInitialResponse | None]


EsiFinding = Literal[
    "indication",
    "conservative_care",
    "imaging",
    "separately_billed_guidance",
    "frequency",
    "region",
    "levels",
    "bilateral_billing",
    "repeat_evidence",
]


class _Instability(TypedDict):
    translation_tenths_mm: int
    angulation_tenths_deg: int


class _Neurology(TypedDict):
    myelopathy_class: int
    progressive_deficit_during_care: bool
    progressive_motor_weakness: bool
    motor_weakness_limits_adls: bool
    cord_related_sphincter_loss: bool
    gait_dexterity_or_sphincter_symptoms: bool


class _PseudarthrosisDetail(TypedDict):
    imaging_nonunion: bool
    months_since_fusion: int
    imaging_hardware_failure: bool


class _Pseudarthrosis(TypedDict):
    symptomatic_pseudarthrosis: _PseudarthrosisDetail


class _DeformityDetail(TypedDict):
    kind: (
        Literal[
            "qualifying_cervical_deformity",
            "post_laminectomy",
            "rheumatoid_arthritis",
            "degenerative_spondylolisthesis",
            "other",
        ]
        | _Pseudarthrosis
    )
    substantial_functional_limitation: bool
    progressing: bool


class _TraumaDetail(TypedDict):
    injury_documented: bool
    likely_instability: bool
    concordant_neurologic_deficit: bool


class _TumorDetail(TypedDict):
    documented: bool
    deficit: bool
    stabilization_needed: bool
    treatment_likely_to_destabilize_or_cause_deficit: bool


class _InfectionDetail(TypedDict):
    confirmed: bool
    vertebral_destruction: bool
    debridement_destabilizes: bool


class _Deformity(TypedDict):
    deformity: _DeformityDetail


class _Trauma(TypedDict):
    trauma: _TraumaDetail


class _Tumor(TypedDict):
    tumor: _TumorDetail


class _Infection(TypedDict):
    infection: _InfectionDetail


class FusionEvidence(TypedDict):
    indication: (
        Literal["nerve_root_impingement", "symptomatic_canal_stenosis"]
        | _Trauma
        | _Tumor
        | _Infection
        | _Deformity
    )
    arm_pain_vas: int
    arm_pain_weeks: int
    failed_multimodal_care: bool
    limits_adls: bool
    other_causes_excluded: bool
    isolated_axial_pain: bool
    mri_or_ct_level_matches: bool
    stenosis: NotRequired[Literal["central", "lateral_recess", "foraminal"] | None]
    imaging_cause: Literal[
        "degenerative_disc_disease",
        "tumor",
        "post_infection",
        "congenital_short_pedicles",
        "ossified_posterior_longitudinal_ligament",
        "cord_compression",
        "instability",
        "other",
    ]
    instability: _Instability
    neurology: _Neurology


FusionFinding = Literal[
    "symptoms", "conservative_care", "other_causes", "imaging", "indication_evidence"
]
