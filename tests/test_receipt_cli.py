import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

FIXTURES = Path(__file__).resolve().parent / "fixtures" / "receipts"


def verify(
    directory: Path, document: bytes, public_key: bytes | None = None
) -> subprocess.CompletedProcess[str]:
    (directory / "receipt.json").write_bytes(document)
    (directory / "public.key").write_bytes(
        (FIXTURES / "public.key").read_bytes() if public_key is None else public_key
    )
    command = Path(sys.executable).with_name(
        "medverify.exe" if os.name == "nt" else "medverify"
    )
    proxies = dict.fromkeys(
        ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"), "http://127.0.0.1:1"
    )
    return subprocess.run(
        [command, "verify-receipt", "receipt.json", "--public-key", "public.key"],
        cwd=directory,
        env={**os.environ, **proxies, "NO_PROXY": "", "UV_OFFLINE": "1"},
        capture_output=True,
        text=True,
        check=False,
        timeout=10,
    )


def test_verifies_offline_and_accepts_reformatted_jcs_payload(tmp_path: Path) -> None:
    document = (FIXTURES / "valid.json").read_bytes()
    receipt = json.loads(document)
    assert set(receipt) == {"payload", "signature"}
    assert set(receipt["payload"]) == {
        "schema_version", "engine_version", "index_digest", "policy_digest",
        "input_commitment", "verdict_commitment", "finding_codes",
    }
    assert receipt["payload"]["finding_codes"] == ["mue_claim_line"]
    reformatted = json.dumps(receipt, indent=2, sort_keys=True).encode()
    assert reformatted != document
    for value in (document, reformatted):
        result = verify(tmp_path, value)
        assert (result.returncode, result.stdout, result.stderr) == (
            0, "Receipt signature verified.\n", ""
        )


def test_rejects_wrong_trusted_key_and_one_bit_mutations(tmp_path: Path) -> None:
    document = (FIXTURES / "valid.json").read_bytes()
    # A different valid Ed25519 public key from RFC 8032, section 7.1.
    wrong_key = b"d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
    receipt = json.loads(document)
    signature = bytearray.fromhex(receipt["signature"])
    signature[0] ^= 1
    receipt["signature"] = signature.hex()
    for result in (
        verify(tmp_path, document, wrong_key),
        verify(tmp_path, (FIXTURES / "tampered.json").read_bytes()),
        verify(tmp_path, json.dumps(receipt).encode()),
    ):
        assert result.returncode == 1
        assert result.stdout == ""
        assert result.stderr == (
            "medverify verify-receipt: receipt signature verification failed\n"
        )


@pytest.mark.parametrize(
    "old,new",
    [
        (b'"schema_version":1', b'"schema_version":1,"schema_version":1'),
        (b'"schema_version":1', b'"schema_version":2'),
        (b'"schema_version":1', b'"schema_version":1,"claim_id":"CANARY_CLAIM"'),
        (b'"mue_claim_line"', b'"CANARY_FREE_TEXT"'),
        (b'"mue_claim_line"', b'"mue_claim_line","mue_claim_line"'),
        (b'"engine_version":"0.2.0"', b'"engine_version":"CANARY_FREE_TEXT"'),
        (b'"payload":', b'"CANARY_FIELD":"CANARY_PATIENT","payload":'),
        (b'"payload":', b'"payload":{},"payload":'),
    ],
)
def test_rejects_non_schema_data_without_echoing_it(
    tmp_path: Path, old: bytes, new: bytes
) -> None:
    document = (FIXTURES / "valid.json").read_bytes()
    assert document.count(old) == 1
    result = verify(tmp_path, document.replace(old, new))
    assert result.returncode == 1
    assert result.stdout == ""
    assert result.stderr == "medverify verify-receipt: invalid receipt schema\n"
