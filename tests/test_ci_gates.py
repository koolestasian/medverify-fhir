from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
WORKFLOW = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")


def _run_line(marker: str) -> list[str]:
    lines = [line for line in WORKFLOW.splitlines() if marker in line]
    assert len(lines) == 1, marker
    return lines[0].split()


def test_stubtest_covers_every_typed_extension_module():
    stubbed = {
        "medverify." + path.stem for path in (ROOT / "python/medverify").glob("*.pyi")
    }
    assert stubbed >= {"medverify._medverify", "medverify.wiser", "medverify.fhir_pas"}
    assert stubbed <= set(_run_line("mypy.stubtest"))


def test_free_threaded_job_runs_every_binding_test_file():
    files = {
        path.relative_to(ROOT).as_posix()
        for path in (ROOT / "tests").glob("test_*bindings.py")
    }
    assert files >= {"tests/test_bindings.py", "tests/test_policy_bindings.py"}
    assert files <= set(_run_line("--python 3.14t pytest"))
