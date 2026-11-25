"""Installed MedVerify command-line entry point."""

import argparse
import json
import sys
from collections.abc import Callable
from pathlib import Path
from tempfile import TemporaryDirectory
from time import perf_counter_ns

from . import ClaimLine, ModifierEvidence, fhir_pas, wiser
from ._medverify import _demo_documents, _demo_engine, _verify_receipt


def _line(
    id: int,
    code: str,
    units: int = 1,
    *,
    modifier: str | None = None,
    evidence: ModifierEvidence | None = None,
) -> ClaimLine:
    return ClaimLine(
        id=id,
        beneficiary="demo-patient",
        billing_provider="demo-provider",
        date="2026-10-01",
        setting="practitioner",
        code=code,
        units=units,
        claim_id="demo-claim",
        modifiers=[modifier] if modifier is not None else None,
        modifier_evidence=[evidence] if evidence is not None else None,
    )


def _trap(
    name: str, finding: str, detect: Callable[[], bool], repair: Callable[[], bool]
) -> None:
    started = perf_counter_ns()
    detected = detect()
    elapsed_us = (perf_counter_ns() - started) / 1_000
    if not detected:
        raise ValueError(f"{name}: expected {finding}")
    if not repair():
        raise ValueError(f"{name}: repaired control failed")
    print(f"{name} | {finding} | {elapsed_us:.1f} us | repaired control passed")


def _demo(directory: Path) -> None:
    engine = _demo_engine(directory)

    def ptp(column_two: ClaimLine, outcome: str) -> bool:
        result = engine.evaluate([_line(1, "Y9001"), column_two])
        return [item["outcome"] for item in result["ptp"]] == [outcome] and not result[
            "mue"
        ]

    _trap(
        "PTP unsupported modifier",
        "supported_modifier_required",
        lambda: ptp(_line(2, "Y9002", modifier="XS"), "supported_modifier_required"),
        lambda: ptp(
            _line(
                2,
                "Y9002",
                modifier="XS",
                evidence=ModifierEvidence(1, "XS", "separate_structure"),
            ),
            "bypassed",
        ),
    )

    def mue(second: ClaimLine, scopes: list[str]) -> bool:
        result = engine.evaluate([_line(1, "Y9011", 2), second])
        return [item["scope"] for item in result["mue"]] == scopes and not result["ptp"]

    _trap(
        "MAI=1 two plus two units",
        "unsupported_split",
        lambda: mue(_line(2, "Y9011", 2), ["unsupported_split"]),
        lambda: mue(
            _line(
                2,
                "Y9011",
                2,
                modifier="76",
                evidence=ModifierEvidence(
                    1, "76", "repeat_procedure_same_practitioner"
                ),
            ),
            [],
        ),
    )

    def utn(loop_id: int) -> wiser.UtnError | None:
        number = "AB120000000004"
        return wiser.validate_utn(
            number,
            "2026-07-06",
            "2026-07-06",
            "office",
            {"electronic837": {"loop_id": loop_id, "qualifier": "G1", "ref02": number}},
        )

    _trap(
        "WISeR misplaced UTN",
        "wrong_placement",
        lambda: utn(2400) == "wrong_placement",
        lambda: utn(2300) is None,
    )

    submission, valid_response = _demo_documents()
    request = fhir_pas.validate_request(submission)
    bad_response = json.loads(valid_response)
    bad_response["entry"][0]["resource"]["item"][0]["itemSequence"] = 999
    bad_document = json.dumps(bad_response)

    def unknown_item() -> bool:
        try:
            fhir_pas.validate_response(bad_document, request)
        except ValueError as error:
            return str(error) == "ItemCorrelation"
        return False

    def correlated() -> bool:
        response = fhir_pas.validate_response(valid_response, request)
        return (
            not response.business_error
            and all(
                response.action(sequence) == "approved"
                for sequence in request.sequences
            )
            and request.sequences == [1, 5]
        )

    _trap("PAS unknown request item", "ItemCorrelation", unknown_item, correlated)


def main() -> int:
    parser = argparse.ArgumentParser(prog="medverify")
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser(
        "demo", help="run four synthetic checks and their repaired controls"
    )
    verify = commands.add_parser(
        "verify-receipt", help="verify a signed receipt with a trusted Ed25519 key"
    )
    verify.add_argument("file", type=Path)
    verify.add_argument("--public-key", type=Path, required=True)
    args = parser.parse_args()
    if args.command == "verify-receipt":
        try:
            _verify_receipt(args.file, args.public_key)
        except ValueError as error:
            print(f"medverify verify-receipt: {error}", file=sys.stderr)
            return 1
        print("Receipt signature verified.")
        return 0
    try:
        with TemporaryDirectory(prefix="medverify-demo-") as directory:
            _demo(Path(directory))
    except (OSError, ValueError) as error:
        print(f"medverify demo: {error}", file=sys.stderr)
        return 1
    return 0
