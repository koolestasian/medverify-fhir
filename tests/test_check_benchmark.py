import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))

import check_benchmark


def index_file(path, *, version, kind, mode, release, counts, payload):
    head = bytearray(64)
    head[:8] = b"MVNCCI01"
    head[8:10] = version.to_bytes(2, "little")
    head[10] = kind
    head[11] = mode
    head[12:16] = (16).to_bytes(4, "little")
    head[16:24] = counts
    head[24 : 24 + len(release)] = release
    path.write_bytes(bytes(head) + b"\0" * payload)


def ptp(directory, rows=10, entries=3, mode=2, release=b"stress01", payload=None):
    counts = rows.to_bytes(4, "little") + entries.to_bytes(4, "little")
    size = 12 * entries + 16 * rows if payload is None else payload
    index_file(
        directory / "ncci_ptp.bin",
        version=2,
        kind=1,
        mode=mode,
        release=release,
        counts=counts,
        payload=size,
    )


def mue(directory, count=4, mode=2, release=b"stress01", payload=None):
    size = 16 * count if payload is None else payload
    index_file(
        directory / "ncci_mue.bin",
        version=1,
        kind=2,
        mode=mode,
        release=release,
        counts=count.to_bytes(8, "little"),
        payload=size,
    )


def test_a_stress_pair_passes(tmp_path, capsys):
    ptp(tmp_path)
    mue(tmp_path)
    check_benchmark.preflight(tmp_path, 10)
    assert "10 PTP and 4 MUE" in capsys.readouterr().out


def test_an_ordinary_mue_catalog_next_to_a_stress_ptp_catalog_is_rejected(tmp_path):
    ptp(tmp_path)
    mue(tmp_path, mode=0, release=b"synth01")
    with pytest.raises(
        SystemExit, match="ncci_mue.bin is not a synthetic-stress catalog"
    ):
        check_benchmark.preflight(tmp_path, 10)


@pytest.mark.parametrize(
    "make, message",
    [
        (
            lambda d: ptp(d, mode=0, release=b"synth01"),
            "ncci_ptp.bin is not a synthetic-stress",
        ),
        (lambda d: ptp(d, rows=9), "ncci_ptp.bin has 9 records, expected 10"),
        (lambda d: ptp(d, payload=10), "ncci_ptp.bin length differs"),
    ],
)
def test_a_wrong_ptp_catalog_is_rejected(tmp_path, make, message):
    make(tmp_path)
    mue(tmp_path)
    with pytest.raises(SystemExit, match=message):
        check_benchmark.preflight(tmp_path, 10)


@pytest.mark.parametrize(
    "make, message",
    [
        (lambda d: mue(d, payload=8), "ncci_mue.bin length differs"),
        (lambda d: mue(d, count=0), "ncci_mue.bin length differs"),
        (lambda d: None, "cannot read"),
    ],
)
def test_a_missing_or_malformed_mue_catalog_is_rejected(tmp_path, make, message):
    ptp(tmp_path)
    make(tmp_path)
    with pytest.raises(SystemExit, match=message):
        check_benchmark.preflight(tmp_path, 10)
