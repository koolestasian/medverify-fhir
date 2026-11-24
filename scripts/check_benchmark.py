import argparse
import json
import math
import os
import subprocess
from pathlib import Path
from tempfile import TemporaryDirectory

import psutil

HEADER = 64
ROW = 16
DIRECTORY_ENTRY = 12
MAGIC = b"MVNCCI01"
STRESS_MODE = 2
STRESS_RELEASE = b"stress01".ljust(8, b"\0")


def preflight(index_dir: Path, expect_ptp_rows: int) -> None:
    """Checks that both files are `synthetic-stress` output before any timing starts.

    The bench later compares every record with the generator; this rejects a wrong or mixed
    catalog pair (such as a stress PTP index next to an ordinary synth01 MUE index) up front.
    """

    def header(name: str) -> tuple[bytes, int]:
        path = index_dir / name
        try:
            with path.open("rb") as stream:
                head = stream.read(HEADER)
        except OSError as error:
            raise SystemExit(f"cannot read {path}: {error.strerror}") from None
        if len(head) != HEADER or head[:8] != MAGIC:
            raise SystemExit(f"{name} is not a MedVerify index")
        if head[11] != STRESS_MODE or head[24:32] != STRESS_RELEASE:
            raise SystemExit(f"{name} is not a synthetic-stress catalog")
        if int.from_bytes(head[12:16], "little") != ROW:
            raise SystemExit(f"{name} has an unexpected record size")
        return head, path.stat().st_size

    ptp, ptp_size = header("ncci_ptp.bin")
    version, kind = int.from_bytes(ptp[8:10], "little"), ptp[10]
    rows = int.from_bytes(ptp[16:20], "little")
    directory = int.from_bytes(ptp[20:24], "little")
    if (version, kind) != (2, 1):
        raise SystemExit("ncci_ptp.bin is not a version 2 PTP index")
    if rows != expect_ptp_rows:
        raise SystemExit(f"ncci_ptp.bin has {rows} records, expected {expect_ptp_rows}")
    if ptp_size != HEADER + DIRECTORY_ENTRY * directory + ROW * rows:
        raise SystemExit("ncci_ptp.bin length differs from its header")

    mue, mue_size = header("ncci_mue.bin")
    version, kind = int.from_bytes(mue[8:10], "little"), mue[10]
    count = int.from_bytes(mue[16:24], "little")
    if (version, kind) != (1, 2):
        raise SystemExit("ncci_mue.bin is not a version 1 MUE index")
    if count == 0 or mue_size != HEADER + ROW * count:
        raise SystemExit("ncci_mue.bin length differs from its header")
    print(f"Preflight: {rows} PTP and {count} MUE synthetic-stress records", flush=True)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--bench", choices=["throughput"], required=True)
    parser.add_argument("--min-claims-per-second", type=int, required=True)
    parser.add_argument("--index-dir", type=Path)
    parser.add_argument("--expect-ptp-rows", type=int)
    args = parser.parse_args()
    if args.min_claims_per_second <= 0:
        parser.error("minimum throughput must be positive")
    if (args.index_dir is None) != (args.expect_ptp_rows is None):
        parser.error("--index-dir and --expect-ptp-rows go together")
    if args.expect_ptp_rows is not None and args.expect_ptp_rows <= 0:
        parser.error("--expect-ptp-rows must be positive")
    names: tuple[str, ...] = ("pair_lookup", "ten_line_claim", "wiser_pas")
    gated = "ten_line_claim"
    env: dict[str, str] = {}
    if args.index_dir is not None:
        assert args.expect_ptp_rows is not None
        preflight(args.index_dir, args.expect_ptp_rows)
        names = ("stress_ten_line_claim",)
        gated = names[0]
        env = {
            "MEDVERIFY_BENCH_INDEX_DIR": str(args.index_dir.resolve()),
            "MEDVERIFY_BENCH_EXPECT_PTP_ROWS": str(args.expect_ptp_rows),
        }

    process = psutil.Process()
    cpu = min(process.cpu_affinity())
    process.cpu_affinity([cpu])
    if process.cpu_affinity() != [cpu]:
        raise SystemExit("failed to pin the benchmark process")
    print(f"Pinned to logical CPU {cpu}", flush=True)

    # A fresh Criterion directory prevents a stale estimate from satisfying the gate.
    with TemporaryDirectory(prefix="medverify-criterion-") as directory:
        subprocess.run(
            [
                "cargo",
                "bench",
                "--locked",
                "-p",
                "medverify-core",
                "--bench",
                args.bench,
                "--",
                "--noplot",
            ],
            check=True,
            env={**os.environ, **env, "CRITERION_HOME": directory},
        )
        for name in names:
            path = Path(directory, name, "new", "estimates.json")
            median_ns = json.loads(path.read_text(encoding="utf-8"))["median"][
                "point_estimate"
            ]
            if not math.isfinite(median_ns) or median_ns <= 0:
                raise SystemExit(f"invalid median for {name}: {median_ns}")
            print(f"{name} median: {median_ns:.2f} ns")
            if name == gated:
                rate = 1e9 / median_ns
                print(f"Complete ten-line claims per second: {rate:,.2f}")
                if rate <= args.min_claims_per_second:
                    raise SystemExit(
                        f"median throughput must exceed {args.min_claims_per_second}"
                    )


if __name__ == "__main__":
    main()
