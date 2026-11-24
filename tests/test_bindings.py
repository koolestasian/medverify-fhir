import os
import shutil
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from threading import Barrier

import pytest
from medverify import ClaimLine, MedVerifyEngine, ModifierEvidence


@pytest.fixture(scope="session")
def indexes(tmp_path_factory):
    destination = tmp_path_factory.mktemp("indexes") / "synthetic"
    root = Path(__file__).resolve().parents[1]
    subprocess.run(
        [
            "cargo",
            "run",
            "--quiet",
            "--locked",
            "-p",
            "medverify-core",
            "--bin",
            "medverify-index",
            "--",
            "synthetic",
            "--data",
            "data",
            "--out",
            str(destination),
        ],
        cwd=root,
        check=True,
    )
    return destination


@pytest.fixture(scope="session")
def engine(indexes):
    return MedVerifyEngine(indexes)


DAY = {
    "beneficiary": "patient-a",
    "billing_provider": "provider-a",
    "date": "2026-10-01",
    "setting": "practitioner",
}


def line(id, code, units=1, **fields):
    return ClaimLine(id=id, code=code, units=units, **(DAY | fields))


def test_synthetic_ptp_target_modifier_evidence_and_liability(engine):
    one = line(10, "Y9001")
    evidence = ModifierEvidence(10, "XS", "separate_structure")
    two = line(
        3,
        "Y9002",
        modifiers=["59"],
        modifier_evidence=[evidence],
        abn_liability_shift_attempted=True,
    )
    expected = {
        "service_day": DAY,
        "column_one_line_id": 10,
        "column_two_line_id": 3,
        "outcome": "supported_modifier_required",
        "abn_cannot_shift_liability": True,
        "modifier_issues": [
            {
                "line_id": 3,
                "modifier": "59",
                "kind": "more_specific_modifier_required",
                "required_modifier": "XS",
            }
        ],
    }
    assert engine.evaluate([two, one]) == {"ptp": [expected], "mue": []}
    two = line(3, "y9002", modifiers=("xs",), modifier_evidence=[evidence])
    assert engine.evaluate([one, two])["ptp"] == [
        expected
        | {
            "outcome": "bypassed",
            "modifier_issues": [],
            "abn_cannot_shift_liability": False,
        }
    ]
    for date, outcome in [
        ("2026-09-30", "modifier_prohibited"),
        ("2026-10-01", "supported_modifier_required"),
    ]:
        pair = [line(1, "Y9001", date=date), line(2, "Y9002", date=date)]
        assert engine.evaluate(pair)["ptp"][0]["outcome"] == outcome


def test_synthetic_mue_scopes_totals_and_group_isolation(engine):
    lines = [
        line(5, "Y9031", 2, abn_liability_shift_attempted=True),
        line(4, "Y9031", 2),
        line(2, "Y9021", 1),
        line(3, "Y9021", 2),
        line(1, "Y9011", 4),
        line(1, "Y9021", 1, beneficiary="different-patient"),
    ]
    assert engine.evaluate(lines) == {
        "ptp": [],
        "mue": [
            {
                "service_day": DAY,
                "code": code,
                "scope": scope,
                "line_id": line_id,
                "claim_id": None,
                "max_units": limit,
                "total_units": total,
                "abn_cannot_shift_liability": abn,
            }
            for code, scope, line_id, limit, total, abn in [
                ("Y9011", "claim_line", 1, 3, 4, False),
                ("Y9021", "absolute_service_day_limit", None, 2, 3, False),
                ("Y9031", "service_day_clinical_review", None, 3, 4, True),
            ]
        ],
    }


def test_batches_preserve_order_and_do_not_combine_service_days(engine):
    a, b = line(1, "Y9021", 2), line(2, "Y9021", 2)
    empty = {"ptp": [], "mue": []}
    combined = engine.evaluate([a, b])
    assert combined["mue"][0]["total_units"] == 4
    assert engine.evaluate_batch([]) == []
    assert engine.evaluate_batch(([a], [a, b], [], [b])) == [
        empty,
        combined,
        empty,
        empty,
    ]


def test_concurrent_calls_share_the_same_engine(engine):
    barrier = Barrier(8)

    def evaluate(i):
        claim = [line(i, "Y9011", 4)]
        barrier.wait(timeout=30)
        return [
            engine.evaluate_batch([[], claim])[1]["mue"][0]["line_id"]
            for _ in range(32)
        ]

    with ThreadPoolExecutor(max_workers=8) as pool:
        assert list(pool.map(evaluate, range(8))) == [[i] * 32 for i in range(8)]


@pytest.mark.parametrize(
    ("fields", "error"),
    [
        ({"code": "bad"}, ValueError),
        ({"units": 0}, ValueError),
        ({"units": -1}, OverflowError),
        ({"date": "2026-02-29"}, ValueError),
        ({"date": "2169-06-06"}, ValueError),
        ({"setting": "unknown"}, ValueError),
        ({"service_kind": "unknown"}, ValueError),
        ({"beneficiary": ""}, ValueError),
        ({"billing_provider": " "}, ValueError),
        ({"claim_id": ""}, ValueError),
        ({"claim_id": " 	"}, ValueError),
        ({"modifiers": ["XYZ"]}, ValueError),
        ({"modifier_evidence": [object()]}, TypeError),
    ],
)
def test_rejects_malformed_line(fields, error):
    with pytest.raises(error):
        ClaimLine(**(DAY | {"id": 1, "code": "Y9011", "units": 1} | fields))


def split(line_id, claim_id, scope, units=2):
    return {
        "service_day": DAY,
        "code": "Y9011",
        "scope": scope,
        "line_id": line_id,
        "claim_id": claim_id,
        "max_units": 3,
        "total_units": units,
        "abn_cannot_shift_liability": False,
    }


def test_claim_id_is_optional_keyword_only_and_validated():
    ClaimLine(**(DAY | {"id": 1, "code": "Y9011", "units": 1}))
    ClaimLine(**(DAY | {"id": 1, "code": "Y9011", "units": 1, "claim_id": None}))
    ClaimLine(**(DAY | {"id": 1, "code": "Y9011", "units": 1, "claim_id": "c-1"}))
    for bad in ["", "  "]:
        with pytest.raises(ValueError, match="claim_id"):
            line(1, "Y9011", claim_id=bad)


def test_unknown_claim_boundary_reports_a_possible_split(engine):
    result = engine.evaluate([line(2, "Y9011", 2), line(1, "Y9011", 2)])
    assert result == {
        "ptp": [],
        "mue": [split(2, None, "possible_unsupported_split")],
    }


def test_known_claim_split_reports_the_claim_id(engine):
    pair = [line(1, "Y9011", 2, claim_id="c-1"), line(2, "Y9011", 2, claim_id="c-1")]
    assert engine.evaluate(pair)["mue"] == [split(2, "c-1", "unsupported_split")]
    other = [line(1, "Y9011", 2, claim_id="c-1"), line(2, "Y9011", 2, claim_id="c-2")]
    assert engine.evaluate(other)["mue"] == []


@pytest.mark.parametrize(
    ("modifier", "basis"),
    [
        ("76", "repeat_procedure_same_practitioner"),
        ("77", "repeat_procedure_other_practitioner"),
    ],
)
def test_justified_repeat_is_not_a_split(engine, modifier, basis):
    repeat = line(
        2,
        "Y9011",
        2,
        claim_id="c-1",
        modifiers=[modifier],
        modifier_evidence=[ModifierEvidence(1, modifier, basis)],
    )
    assert engine.evaluate([line(1, "Y9011", 2, claim_id="c-1"), repeat])["mue"] == []
    wrong = line(
        2,
        "Y9011",
        2,
        claim_id="c-1",
        modifiers=[modifier],
        modifier_evidence=[ModifierEvidence(1, modifier, "distinct_service")],
    )
    result = engine.evaluate([line(1, "Y9011", 2, claim_id="c-1"), wrong])
    assert result["mue"] == [split(2, "c-1", "unsupported_split")]


def test_modifier_59_does_not_support_a_repeat_on_an_em_line(engine):
    evidence = ModifierEvidence(1, "59", "distinct_service")

    def pair(**fields):
        return [
            line(1, "Y9011", 2, claim_id="c-1", **fields),
            line(
                2,
                "Y9011",
                2,
                claim_id="c-1",
                modifiers=["59"],
                modifier_evidence=[evidence],
                **fields,
            ),
        ]

    em = pair(service_kind="evaluation_and_management")
    assert engine.evaluate(em)["mue"] == [split(2, "c-1", "unsupported_split")]
    # The same lines for another kind of service keep the bypass.
    assert engine.evaluate(pair())["mue"] == []
    assert engine.evaluate(pair(service_kind="other"))["mue"] == []


def test_evidence_naming_a_line_of_another_claim_does_not_clear_a_split(engine):
    evidence = ModifierEvidence(3, "76", "repeat_procedure_same_practitioner")
    claim_a = [
        line(1, "Y9011", 2, claim_id="c-1"),
        line(
            2,
            "Y9011",
            2,
            claim_id="c-1",
            modifiers=["76"],
            modifier_evidence=[evidence],
        ),
    ]
    expected = [split(2, "c-1", "unsupported_split")]
    assert engine.evaluate(claim_a)["mue"] == expected
    with_claim_b = [*claim_a, line(3, "Y9011", 2, claim_id="c-2")]
    assert engine.evaluate(with_claim_b)["mue"] == expected


def test_repeats_that_justify_each_other_in_a_cycle_do_not_clear_a_split(engine):
    basis = "repeat_procedure_same_practitioner"
    for claim, scope in [
        ("c-1", "unsupported_split"),
        (None, "possible_unsupported_split"),
    ]:
        cycle = [
            line(
                1,
                "Y9011",
                2,
                claim_id=claim,
                modifiers=["76"],
                modifier_evidence=[ModifierEvidence(2, "76", basis)],
            ),
            line(
                2,
                "Y9011",
                2,
                claim_id=claim,
                modifiers=["76"],
                modifier_evidence=[ModifierEvidence(1, "76", basis)],
            ),
        ]
        assert engine.evaluate(cycle)["mue"] == [split(2, claim, scope)]
        assert engine.evaluate(cycle[::-1])["mue"] == [split(2, claim, scope)]
    # A repeat of a plain first line is still accepted.
    chain = [
        line(1, "Y9011", 2, claim_id="c-1"),
        line(
            2,
            "Y9011",
            2,
            claim_id="c-1",
            modifiers=["76"],
            modifier_evidence=[ModifierEvidence(1, "76", basis)],
        ),
    ]
    assert engine.evaluate(chain)["mue"] == []


@pytest.mark.parametrize(
    ("modifier", "basis"),
    [
        ("76", "repeat_procedure_same_practitioner"),
        ("77", "repeat_procedure_other_practitioner"),
        ("91", "repeat_clinical_laboratory_test"),
    ],
)
def test_repeat_modifiers_do_not_support_a_repeat_on_an_em_line(engine, modifier, basis):
    evidence = ModifierEvidence(1, modifier, basis)

    def pair(**fields):
        return [
            line(1, "Y9011", 2, claim_id="c-1", **fields),
            line(
                2,
                "Y9011",
                2,
                claim_id="c-1",
                modifiers=[modifier],
                modifier_evidence=[evidence],
                **fields,
            ),
        ]

    em = pair(service_kind="evaluation_and_management")
    assert engine.evaluate(em)["mue"] == [split(2, "c-1", "unsupported_split")]
    # The same lines for another kind of service keep the bypass.
    assert engine.evaluate(pair())["mue"] == []
    assert engine.evaluate(pair(service_kind="other"))["mue"] == []


def test_lines_separated_only_from_one_anchor_do_not_clear_a_split(engine):
    site = ModifierEvidence(1, "RT", "anatomic_site")

    def right_side(line_id, documented):
        return line(
            line_id,
            "Y9011",
            1,
            claim_id="c-1",
            modifiers=["RT"],
            modifier_evidence=documented,
        )

    anchored = [line(1, "Y9011", 1, claim_id="c-1"), right_side(2, [site]), right_side(3, [site])]
    assert engine.evaluate(anchored)["mue"] == [split(3, "c-1", "unsupported_split", units=1)]
    reciprocal_anchor = line(
        1,
        "Y9011",
        1,
        claim_id="c-1",
        modifiers=["LT"],
        modifier_evidence=[ModifierEvidence(2, "LT", "anatomic_site")],
    )
    assert engine.evaluate([reciprocal_anchor, anchored[1]])["mue"] == []
    assert engine.evaluate([reciprocal_anchor, *anchored[1:]])["mue"] == [
        split(3, "c-1", "unsupported_split", units=1),
    ]
    assert engine.evaluate([reciprocal_anchor, anchored[1], line(3, "Y9011", 1)])["mue"] == [
        split(3, None, "possible_unsupported_split", units=1),
    ]
    # Evidence that the two right-side lines are separate from each other clears both.
    left = line(
        3,
        "Y9011",
        1,
        claim_id="c-1",
        modifiers=["LT"],
        modifier_evidence=[
            ModifierEvidence(1, "LT", "anatomic_site"),
            ModifierEvidence(2, "LT", "anatomic_site"),
        ],
    )
    assert engine.evaluate([*anchored[:2], left])["mue"] == []


def claims_for(count):
    return [
        [line(1, "Y9011", 2), line(2, "Y9011", 2), line(3, "Y9021", 1 + i % 3)]
        if i % 2
        else [line(1, "Y9031", 2 + i % 3, claim_id="a"), line(2, "Y9031", 2)]
        for i in range(count)
    ]


@pytest.mark.parametrize("count", [0, 63, 64, 65])
def test_batch_matches_individual_evaluation(engine, count):
    claims = claims_for(count)
    assert engine.evaluate_batch(claims) == [engine.evaluate(c) for c in claims]


def test_batch_reports_the_lowest_failing_index(engine):
    claims = claims_for(100)
    claims[70] = [line(1, "Y9021", 2**64 - 1), line(2, "Y9021")]
    claims[10] = [line(4, "Y9011"), line(4, "Y9011")]
    with pytest.raises(ValueError, match=r"^claims\[10\]: duplicate claim line ID 4"):
        engine.evaluate_batch(claims)
    del claims[10]
    with pytest.raises(OverflowError, match=r"^claims\[69\]: .*exceeds u64"):
        engine.evaluate_batch(claims)
    with pytest.raises(OverflowError, match=r"^claims\[1\]: "):
        engine.evaluate_batch([[], claims[69], claims[0]])
    with pytest.raises(ValueError) as caught:
        engine.evaluate([line(4, "Y9011"), line(4, "Y9011")])
    assert not str(caught.value).startswith("claims[")


def test_concurrent_large_batches_share_one_engine(engine):
    claims = claims_for(128)
    expected = [engine.evaluate(c) for c in claims]
    barrier = Barrier(8)

    def run(_):
        barrier.wait(timeout=30)
        return engine.evaluate_batch(claims)

    with ThreadPoolExecutor(max_workers=8) as pool:
        assert list(pool.map(run, range(8))) == [expected] * 8


def test_failed_batch_releases_engine_for_reuse(engine):
    a = line(1, "Y9021", 2**64 - 1)
    with pytest.raises(OverflowError, match="exceeds u64"):
        engine.evaluate_batch([[], [a, line(2, "Y9021")]])
    with pytest.raises(ValueError, match="duplicate claim line ID"):
        engine.evaluate([a, a])
    with pytest.raises(TypeError):
        engine.evaluate_batch([[object()]])
    with pytest.raises(ValueError, match="evidence basis"):
        ModifierEvidence(1, "XS", "unknown")
    with pytest.raises(AttributeError):
        engine.index_dir = "elsewhere"
    assert engine.evaluate([]) == {"ptp": [], "mue": []}


def test_index_errors_and_immutable_snapshot(indexes, tmp_path):
    with pytest.raises(FileNotFoundError):
        MedVerifyEngine(tmp_path)
    shutil.copytree(indexes, tmp_path, dirs_exist_ok=True)
    snapshot = MedVerifyEngine(tmp_path)
    path = tmp_path / "ncci_mue.bin"
    data = bytearray(path.read_bytes())
    data[-1] ^= 1
    path.write_bytes(data)
    with pytest.raises(OSError):
        MedVerifyEngine(tmp_path)
    assert snapshot.evaluate([line(1, "Y9011", 4)])["mue"][0]["max_units"] == 3


def test_import_preserves_interpreter_gil_mode():
    # A fresh process observes the transition even if pytest already imported the module.
    # Let CPython decide whether importing an extension requires enabling the GIL.
    env = os.environ.copy()
    env.pop("PYTHON_GIL", None)
    subprocess.run(
        [
            sys.executable,
            "-I",
            "-Werror",
            "-c",
            """
import sys
import sysconfig
if sysconfig.get_config_var("Py_GIL_DISABLED"):
    assert not sys._is_gil_enabled()
    import medverify._medverify
    assert not sys._is_gil_enabled()
else:
    import medverify._medverify
""",
        ],
        check=True,
        env=env,
    )
