use medverify_core::claim::ProcedureCode;
use medverify_core::wiser::*;
use time::macros::date;

fn code(value: &str) -> ProcedureCode {
    ProcedureCode::new(value).unwrap()
}

fn service(value: &str) -> Service<'static> {
    Service {
        state: State::WA,
        plan: Plan::OriginalMedicare,
        site: Site::Office,
        date: date!(2026 - 07 - 06),
        emergency: false,
        inpatient_only: false,
        subject_to_other_cms_prior_authorization: false,
        provider_under_upic_review: false,
        facility_claim: false,
        code: code(value),
        diagnoses: &["G40.919", "N39.41", "G47.33"],
        same_claim_codes: &[],
        individual_npi: Npi::new("1000000001"),
        exemption: None,
    }
}

#[test]
fn washington_routes_twelve_categories_and_three_inactive_or_delayed_categories() {
    use Category::*;
    let policy = Policy::washington_synthetic();
    for (value, category) in [
        ("Y9001", KneeArthroscopy),
        ("Y9002", InducedNerveLesions),
        ("Y9003", VagusNerveStimulation),
        ("Y9004", PhrenicNerveStimulation),
        ("Y9005", ElectricalNerveStimulation),
        ("Y9006", IncontinenceDevices),
        ("Y9007", SacralNerveStimulation),
        ("Y9008", ImpotenceTreatment),
        ("Y9009", VertebralAugmentation),
        ("Y9010", EpiduralSteroidInjection),
        ("Y9011", CervicalFusion),
        ("Y9012", HypoglossalNerveStimulation),
    ] {
        let service = service(value);
        assert_eq!(
            policy.route(
                &service,
                Stage::Planning {
                    as_of: date!(2026 - 01 - 05),
                    elect_prior_authorization: true
                }
            ),
            Route::VoluntaryPriorAuthorization(category)
        );
        assert_eq!(
            policy.route(&service, Stage::Claim(None)),
            Route::PrePaymentReview(category)
        );
    }
    for (value, route) in [
        ("Y9013", Route::Inactive(SkinSubstitutes)),
        ("Y9014", Route::Delayed(DeepBrainStimulation)),
        ("Y9015", Route::Delayed(PercutaneousLumbarDecompression)),
        ("Y9017", Route::OutOfScope),
    ] {
        assert_eq!(policy.route(&service(value), Stage::Claim(None)), route);
    }
}

#[test]
fn routing_respects_dates_plan_state_indications_and_claim_decision() {
    let policy = Policy::washington_synthetic();
    let service = service("Y9012");
    let category = Category::HypoglossalNerveStimulation;
    for excluded in [
        Service {
            state: State::AZ,
            ..service
        },
        Service {
            plan: Plan::Other,
            ..service
        },
        Service {
            plan: Plan::RailroadMedicare,
            ..service
        },
        Service {
            site: Site::Asc,
            ..service
        },
        Service {
            site: Site::HospitalOutpatient,
            ..service
        },
        Service {
            site: Site::Inpatient,
            ..service
        },
        Service {
            emergency: true,
            ..service
        },
        Service {
            inpatient_only: true,
            ..service
        },
        Service {
            subject_to_other_cms_prior_authorization: true,
            ..service
        },
        Service {
            provider_under_upic_review: true,
            ..service
        },
        Service {
            diagnoses: &["G40.919"],
            ..service
        },
        Service {
            date: date!(2026 - 01 - 14),
            ..service
        },
        Service {
            date: date!(2032 - 01 - 01),
            ..service
        },
    ] {
        assert_eq!(
            policy.route(&excluded, Stage::Claim(None)),
            Route::OutOfScope
        );
    }
    assert_eq!(
        policy.route(
            &Service {
                site: Site::Asc,
                facility_claim: true,
                ..service
            },
            Stage::Claim(None)
        ),
        Route::PrePaymentReview(category)
    );
    assert_eq!(
        policy.route(
            &Service {
                date: date!(2026 - 01 - 15),
                ..service
            },
            Stage::Planning {
                as_of: date!(2026 - 01 - 04),
                elect_prior_authorization: true
            }
        ),
        Route::PrePaymentReview(category)
    );
    assert_eq!(
        policy.route(
            &service,
            Stage::Planning {
                as_of: date!(2026 - 01 - 05),
                elect_prior_authorization: false
            }
        ),
        Route::PrePaymentReview(category)
    );
    assert_eq!(
        policy.route(
            &Service {
                date: date!(2031 - 12 - 31),
                ..service
            },
            Stage::Claim(Some(PriorDecision::NonAffirmation))
        ),
        Route::PriorAuthorizationDecision(category, PriorDecision::NonAffirmation)
    );
}

#[test]
fn appendix_pairs_qualify_and_associated_denials_follow_their_primary() {
    let policy = Policy::washington_synthetic();
    let lines = [
        AdjudicatedLine {
            sequence: 1,
            code: code("Y9010"),
            wiser_denied: true,
        },
        AdjudicatedLine {
            sequence: 2,
            code: code("Y9016"),
            wiser_denied: false,
        },
        AdjudicatedLine {
            sequence: 3,
            code: code("Y9011"),
            wiser_denied: false,
        },
        AdjudicatedLine {
            sequence: 4,
            code: code("Y9017"),
            wiser_denied: false,
        },
    ];
    assert_eq!(policy.associated_denials(&lines), [2]);
    assert!(policy.associated_denials(&lines[1..]).is_empty());

    let table = include_str!("../../../policy/wa_wiser.toml").replace(
        "primary = \"Y9010\"\nassociated = \"Y9016\"",
        "primary = \"Y9010\"\nassociated = \"Y9016\"\nrequired_pair = true",
    );
    let policy = Policy::from_toml(&table).unwrap();
    let service = service("Y9010");
    assert_eq!(
        policy.route(&service, Stage::Claim(None)),
        Route::OutOfScope
    );
    assert_eq!(
        policy.route(
            &Service {
                same_claim_codes: &[code("Y9016")],
                ..service
            },
            Stage::Claim(None)
        ),
        Route::PrePaymentReview(Category::EpiduralSteroidInjection)
    );
    let bad_table = table.replace("percent = 80", "percent = 101");
    assert!(matches!(
        Policy::from_toml(&bad_table),
        Err(PolicyError::InvalidThreshold)
    ));
}

#[test]
fn gold_card_uses_individual_latest_qualifying_requests_and_the_dated_threshold() {
    let policy = Policy::washington_synthetic();
    let npi = Npi::new("1000000001").unwrap();
    let other = Npi::new("1000000002").unwrap();
    let first = AssessmentPeriod {
        from: date!(2026 - 01 - 05),
        through: date!(2026 - 07 - 31),
        eligibility_date: date!(2026 - 10 - 01),
    };
    let second = AssessmentPeriod {
        through: date!(2026 - 10 - 31),
        eligibility_date: date!(2027 - 01 - 01),
        ..first
    };
    let make_request = |request_id, outcome| PriorRequest {
        npi,
        request_id,
        submitted: date!(2026 - 07 - 10),
        revision: 1,
        outcome,
    };
    use AssessmentOutcome::*;
    let mut requests: Vec<_> = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "10"]
        .into_iter()
        .enumerate()
        .map(|(position, id)| make_request(id, if position < 8 { Affirmed } else { NonAffirmed }))
        .collect();
    assert!(!policy.gold_card_eligible(npi, first, &requests[..9]));
    requests.push(make_request("dismissed", Dismissed));
    assert_eq!(
        policy
            .gold_card_rule(first.eligibility_date)
            .unwrap()
            .percent,
        80
    );
    assert_eq!(
        policy
            .gold_card_rule(second.eligibility_date)
            .unwrap()
            .percent,
        90
    );
    assert!(policy.gold_card_eligible(npi, first, &requests));
    assert!(!policy.gold_card_eligible(npi, second, &requests));
    let update = PriorRequest {
        revision: 2,
        ..make_request("10", Affirmed)
    };
    requests.push(PriorRequest {
        npi: other,
        ..update
    });
    assert!(!policy.gold_card_eligible(npi, second, &requests));
    requests.push(update);
    assert!(policy.gold_card_eligible(npi, second, &requests));
    requests.push(PriorRequest {
        outcome: NonAffirmed,
        ..update
    });
    assert!(!policy.gold_card_eligible(npi, second, &requests));
}

#[test]
fn only_a_current_matching_virtix_exemption_changes_routing() {
    let policy = Policy::washington_synthetic();
    let service = service("Y9010");
    let npi = service.individual_npi.unwrap();
    let notice = Exemption {
        npi,
        issuer: "Virtix Health",
        notice_id: "synthetic-1",
        effective: date!(2026 - 07 - 06),
        through: date!(2027 - 07 - 05),
    };
    let route = |exemption| {
        policy.route(
            &Service {
                exemption,
                ..service
            },
            Stage::Claim(None),
        )
    };
    let category = Category::EpiduralSteroidInjection;
    assert_eq!(route(Some(notice)), Route::Exempt(category));
    for invalid in [
        None,
        Some(Exemption {
            issuer: "Noridian",
            ..notice
        }),
        Some(Exemption {
            notice_id: "",
            ..notice
        }),
        Some(Exemption {
            effective: date!(2026 - 07 - 07),
            ..notice
        }),
        Some(Exemption {
            through: date!(2026 - 07 - 05),
            ..notice
        }),
        Some(Exemption {
            npi: Npi::new("1000000002").unwrap(),
            ..notice
        }),
    ] {
        assert_eq!(route(invalid), Route::PrePaymentReview(category));
    }
}

#[test]
fn utn_has_fourteen_ascii_bytes_and_counts_the_decision_as_day_one() {
    let number = "AB120000000004";
    let utn = Utn::new(number).unwrap();
    assert!(Utn::new("A120000000004").is_none());
    assert!(Utn::new("é120000000004").is_none());
    assert!(Utn::new("AB12000000000 ").is_none());
    let decision = date!(2026 - 01 - 05);
    assert!(utn.valid_on(decision, decision));
    assert!(utn.valid_on(decision, date!(2026 - 05 - 04)));
    assert!(!utn.valid_on(decision, date!(2026 - 05 - 05)));
    assert!(!utn.valid_on(decision, date!(2026 - 01 - 04)));
}

#[test]
fn utn_placement_depends_on_channel_and_claim_role() {
    let number = "AB120000000004";
    let utn = Utn::new(number).unwrap();
    let day = date!(2026 - 07 - 06);
    let check = |site, field: UtnField<'_>| validate_utn(utn, day, day, site, field);
    let electronic = UtnField::Electronic837 {
        loop_id: 2300,
        qualifier: "G1",
        ref02: number,
    };
    assert_eq!(check(Site::Office, electronic), Ok(()));
    assert_eq!(check(Site::Asc, electronic), Err(UtnError::WrongClaim));
    assert_eq!(
        check(
            Site::Asc,
            UtnField::AscFacility837 {
                loop_id: 2300,
                qualifier: "G1",
                ref02: number
            }
        ),
        Ok(())
    );
    assert_eq!(
        check(
            Site::Office,
            UtnField::Electronic837 {
                loop_id: 2400,
                qualifier: "G1",
                ref02: number
            }
        ),
        Err(UtnError::WrongPlacement)
    );
    assert_eq!(
        check(
            Site::Office,
            UtnField::Electronic837 {
                loop_id: 2300,
                qualifier: "9F",
                ref02: number
            }
        ),
        Err(UtnError::WrongPlacement)
    );
    assert_eq!(
        check(
            Site::Office,
            UtnField::PaperCms1500Item23(&format!("{number}other"))
        ),
        Ok(())
    );
    assert_eq!(
        check(
            Site::HospitalOutpatient,
            UtnField::ElectronicInstitutionalTreatmentAuthorization(&format!("{number}    "))
        ),
        Ok(())
    );
    assert_eq!(
        check(
            Site::HospitalOutpatient,
            UtnField::ManualInstitutionalTreatmentAuthorization(&format!("{:18}{number}", ""))
        ),
        Ok(())
    );
    assert!(check(
        Site::HospitalOutpatient,
        UtnField::ManualInstitutionalTreatmentAuthorization(&format!("{number}{:18}", ""))
    )
    .is_err());
    assert_eq!(
        check(Site::Office, UtnField::PaperCms1500Item23("AB120000000005")),
        Err(UtnError::WrongUtn)
    );
}

const LINE: EsiLine = EsiLine {
    region: Region::LumbarSacral,
    level: 1,
    laterality: Laterality::None,
    units: 1,
};

fn esi() -> EsiEvidence<'static> {
    EsiEvidence {
        date: date!(2026 - 07 - 06),
        region: Region::LumbarSacral,
        approach: Approach::Transforaminal,
        indication: EsiIndication::RadicularSymptoms,
        supporting_exam_and_imaging: true,
        functional_impact_and_baseline: true,
        pain_weeks: 4,
        conservative_care: ConservativeCare::FailedAfterWeeks(4),
        active_rehabilitation: true,
        guidance: ImagingGuidance::CtOrFluoroscopyWithContrast,
        separately_billed_guidance: false,
        bilateral_indicated: false,
        billing: EsiBilling::Professional,
        lines: &[LINE],
        history: &[],
        repeat: None,
    }
}

#[test]
fn esi_counts_distinct_sessions_in_one_region_over_a_rolling_year() {
    let request = esi();
    let same = |date| EsiSession {
        date,
        region: request.region,
    };
    let history = [
        same(date!(2025 - 07 - 06)),
        same(date!(2026 - 01 - 01)),
        same(date!(2026 - 01 - 01)),
        same(date!(2026 - 02 - 01)),
        EsiSession {
            date: date!(2026 - 02 - 01),
            region: Region::CervicalThoracic,
        },
        same(date!(2026 - 03 - 01)),
    ];
    let repeat = Some(RepeatEsi::SustainedImprovement {
        percent: 50,
        months: 3,
        same_scale: true,
    });
    assert!(evaluate_esi(&EsiEvidence {
        history: &history,
        repeat,
        ..request
    })
    .is_empty());
    let mut frequency_fail = history.to_vec();
    frequency_fail.push(same(date!(2026 - 04 - 01)));
    assert_eq!(
        evaluate_esi(&EsiEvidence {
            history: &frequency_fail,
            repeat,
            ..request
        }),
        [EsiFinding::Frequency]
    );
    let same_day_other_region = [EsiSession {
        date: request.date,
        region: Region::CervicalThoracic,
    }];
    assert_eq!(
        evaluate_esi(&EsiEvidence {
            history: &same_day_other_region,
            ..request
        }),
        [EsiFinding::Region]
    );
}

#[test]
fn esi_checks_levels_conservative_care_imaging_and_bilateral_billing() {
    let request = esi();
    assert!(evaluate_esi(&request).is_empty());
    let right = EsiLine {
        laterality: Laterality::Right,
        ..LINE
    };
    let left = EsiLine {
        laterality: Laterality::Left,
        ..LINE
    };
    let bilateral = EsiLine {
        laterality: Laterality::Bilateral50,
        ..LINE
    };
    let right_left = [right, left];
    let facility = EsiEvidence {
        billing: EsiBilling::AscFacility,
        bilateral_indicated: true,
        lines: &right_left,
        ..request
    };
    assert!(evaluate_esi(&facility).is_empty());
    let one_bilateral = [bilateral];
    let professional = EsiEvidence {
        bilateral_indicated: true,
        lines: &one_bilateral,
        ..request
    };
    assert!(evaluate_esi(&professional).is_empty());
    assert_eq!(
        evaluate_esi(&EsiEvidence {
            lines: &one_bilateral,
            ..facility
        }),
        [EsiFinding::BilateralBilling]
    );
    assert_eq!(
        evaluate_esi(&EsiEvidence {
            approach: Approach::Interlaminar,
            ..professional
        }),
        [EsiFinding::BilateralBilling]
    );

    let levels = [
        LINE,
        EsiLine {
            level: 2,
            region: Region::CervicalThoracic,
            ..LINE
        },
        EsiLine { level: 3, ..LINE },
    ];
    assert_eq!(
        evaluate_esi(&EsiEvidence {
            lines: &levels,
            ..request
        }),
        [EsiFinding::Region, EsiFinding::Levels]
    );
    assert_eq!(
        evaluate_esi(&EsiEvidence {
            conservative_care: ConservativeCare::FailedAfterWeeks(3),
            guidance: ImagingGuidance::Other,
            separately_billed_guidance: true,
            ..request
        }),
        [
            EsiFinding::ConservativeCare,
            EsiFinding::Imaging,
            EsiFinding::SeparatelyBilledGuidance
        ]
    );
    assert!(evaluate_esi(&EsiEvidence {
        indication: EsiIndication::AcuteHerpesZoster,
        pain_weeks: 1,
        conservative_care: ConservativeCare::RefractoryZoster,
        guidance: ImagingGuidance::UltrasoundWithoutContrast {
            documented_allergy_or_pregnancy: true
        },
        ..request
    })
    .is_empty());
}

#[test]
fn esi_repeat_after_poor_response_requires_fourteen_days_and_documented_change() {
    let evidence = esi();
    let assess = |initial_date| {
        evaluate_esi(&EsiEvidence {
            repeat: Some(RepeatEsi::PoorInitialResponse {
                initial_date,
                changed_approach_level_or_drug: true,
                documented_rationale: true,
            }),
            ..evidence
        })
    };
    assert_eq!(assess(date!(2026 - 06 - 23)), [EsiFinding::RepeatEvidence]);
    assert!(assess(date!(2026 - 06 - 22)).is_empty());
}

fn fusion() -> FusionEvidence {
    FusionEvidence {
        indication: FusionIndication::NerveRootImpingement,
        arm_pain_vas: 4,
        arm_pain_weeks: 12,
        failed_multimodal_care: true,
        limits_adls: true,
        other_causes_excluded: true,
        isolated_axial_pain: false,
        mri_or_ct_level_matches: true,
        stenosis: Some(Stenosis::Foraminal),
        imaging_cause: ImagingCause::DegenerativeDiscDisease,
        instability: Instability::default(),
        neurology: Neurology::default(),
    }
}

#[test]
fn cervical_nerve_root_and_canal_have_distinct_symptom_and_imaging_requirements() {
    let root = fusion();
    assert!(evaluate_fusion(root).is_empty());
    assert_eq!(
        evaluate_fusion(FusionEvidence {
            arm_pain_weeks: 11,
            ..root
        }),
        [FusionFinding::ConservativeCare]
    );
    assert!(evaluate_fusion(FusionEvidence {
        arm_pain_weeks: 1,
        arm_pain_vas: 7,
        ..root
    })
    .is_empty());
    assert_eq!(
        evaluate_fusion(FusionEvidence {
            arm_pain_weeks: 1,
            arm_pain_vas: 7,
            mri_or_ct_level_matches: false,
            ..root
        }),
        [FusionFinding::ConservativeCare, FusionFinding::Imaging]
    );
    assert!(evaluate_fusion(FusionEvidence {
        arm_pain_weeks: 1,
        neurology: Neurology {
            myelopathy_class: 3,
            ..Default::default()
        },
        ..root
    })
    .is_empty());
    let canal = FusionEvidence {
        indication: FusionIndication::SymptomaticCanalStenosis,
        arm_pain_weeks: 1,
        imaging_cause: ImagingCause::CordCompression,
        ..root
    };
    assert_eq!(evaluate_fusion(canal), [FusionFinding::Imaging]);
    let central = FusionEvidence {
        stenosis: Some(Stenosis::Central),
        ..canal
    };
    assert!(evaluate_fusion(central).is_empty());
    assert_eq!(
        evaluate_fusion(FusionEvidence {
            limits_adls: false,
            ..central
        }),
        [FusionFinding::ConservativeCare]
    );
    assert_eq!(
        evaluate_fusion(FusionEvidence {
            isolated_axial_pain: true,
            ..central
        }),
        [FusionFinding::Symptoms]
    );
}

#[test]
fn cervical_stabilization_requires_evidence_for_its_own_indication() {
    let root = fusion();
    let trauma = FusionEvidence {
        indication: FusionIndication::Trauma {
            injury_documented: true,
            likely_instability: false,
            concordant_neurologic_deficit: false,
        },
        arm_pain_weeks: 0,
        ..root
    };
    let exact = Instability {
        translation_tenths_mm: 35,
        angulation_tenths_deg: 110,
    };
    for (instability, expected) in [
        (exact, vec![FusionFinding::IndicationEvidence]),
        (
            Instability {
                translation_tenths_mm: 36,
                ..exact
            },
            vec![],
        ),
        (
            Instability {
                angulation_tenths_deg: 111,
                ..exact
            },
            vec![],
        ),
    ] {
        assert_eq!(
            evaluate_fusion(FusionEvidence {
                instability,
                ..trauma
            }),
            expected
        );
    }
    for indication in [
        FusionIndication::Tumor {
            documented: true,
            deficit: true,
            stabilization_needed: true,
            treatment_likely_to_destabilize_or_cause_deficit: false,
        },
        FusionIndication::Infection {
            confirmed: true,
            vertebral_destruction: true,
            debridement_destabilizes: false,
        },
        FusionIndication::Deformity {
            kind: Deformity::SymptomaticPseudarthrosis {
                imaging_nonunion: true,
                months_since_fusion: 12,
                imaging_hardware_failure: false,
            },
            substantial_functional_limitation: false,
            progressing: false,
        },
    ] {
        assert!(evaluate_fusion(FusionEvidence {
            indication,
            ..trauma
        })
        .is_empty());
    }
    assert_eq!(
        evaluate_fusion(FusionEvidence {
            indication: FusionIndication::Infection {
                confirmed: false,
                vertebral_destruction: true,
                debridement_destabilizes: true
            },
            ..trauma
        }),
        [FusionFinding::IndicationEvidence]
    );
}

#[test]
fn diagnosis_matching_ignores_dots_and_case() {
    let policy = Policy::washington_synthetic();
    let dotted = service("Y9012");
    let expected = policy.route(&dotted, Stage::Claim(None));
    assert_ne!(expected, Route::OutOfScope);
    for diagnoses in [&["G4733"][..], &["g4733"], &["g47.33"]] {
        let service = Service {
            diagnoses,
            ..dotted
        };
        assert_eq!(policy.route(&service, Stage::Claim(None)), expected);
    }
    let other = Service {
        diagnoses: &["G40919"],
        ..dotted
    };
    assert_eq!(policy.route(&other, Stage::Claim(None)), Route::OutOfScope);

    let table = include_str!("../../../policy/wa_wiser.toml").replace("G47.33", ".");
    let blank = Policy::from_toml(&table).unwrap();
    let service = Service {
        diagnoses: &["."],
        ..dotted
    };
    assert_eq!(blank.route(&service, Stage::Claim(None)), Route::OutOfScope);
}
