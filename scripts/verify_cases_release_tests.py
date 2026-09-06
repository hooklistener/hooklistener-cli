#!/usr/bin/env python3
"""Gate saved-case coverage, then run the existing full platform conformance suite."""

import hashlib
import json
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EXPECTED_INVENTORY = ROOT / "fixtures/cases_release_test_inventory.txt"
CARGO_TEST = [
    "cargo",
    "test",
    "--release",
    "--locked",
    "--test",
    "tunnel_phase1_conformance",
]
TIMEOUT_SECONDS = 1200


def load_inventory(path: Path) -> list[str]:
    names = path.read_text(encoding="utf-8").splitlines()
    if (
        not names
        or names != sorted(set(names))
        or any(not re.fullmatch(r"cases::[a-z][a-z0-9_]*", name) for name in names)
    ):
        raise ValueError(
            "saved-case inventory must be nonempty, unique, sorted cases:: test names"
        )
    return names


def list_case_tests(*flags: str) -> set[str]:
    listing = subprocess.run(
        [*CARGO_TEST, "--", *flags, "--list"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
        timeout=TIMEOUT_SECONDS,
    )
    sys.stderr.write(listing.stderr)
    return {
        line.removesuffix(": test")
        for line in listing.stdout.splitlines()
        if line.startswith("cases::") and line.endswith(": test")
    }


def binary_digest(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as binary:
        for chunk in iter(lambda: binary.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def receipt_provenance() -> tuple[Path, dict]:
    binary = os.environ.get("HOOKLISTENER_CONFORMANCE_BINARY")
    if not binary:
        raise ValueError("receipt output requires HOOKLISTENER_CONFORMANCE_BINARY")
    binary_path = Path(binary).resolve(strict=True)
    head = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
        timeout=30,
    ).stdout.strip()
    expected_sha = os.environ.get("HOOKLISTENER_CLI_GIT_SHA", head)
    if (
        not re.fullmatch(r"[0-9a-fA-F]{40}", expected_sha)
        or expected_sha.lower() != head
    ):
        raise ValueError(
            "case conformance source SHA does not match the checked-out commit"
        )
    dirty = subprocess.run(
        ["git", "status", "--porcelain"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
        timeout=30,
    ).stdout.strip()
    platform = os.environ.get(
        "HOOKLISTENER_CONFORMANCE_PLATFORM",
        {"darwin": "macos", "win32": "windows"}.get(sys.platform, sys.platform),
    )
    return binary_path, {
        "platform": platform,
        "cli_git_sha": head,
        "source_dirty": bool(dirty),
        "binary_sha256": binary_digest(binary_path),
        "run_id": os.environ.get("GITHUB_RUN_ID"),
        "run_attempt": os.environ.get("GITHUB_RUN_ATTEMPT"),
    }


def main() -> int:
    output_value = os.environ.get("HOOKLISTENER_CASES_CONFORMANCE_OUTPUT")
    output = Path(output_value) if output_value else None
    try:
        # A failed rerun must not leave behind an earlier passed receipt.
        if output:
            output.unlink(missing_ok=True)
        expected = load_inventory(EXPECTED_INVENTORY)
        actual = list_case_tests()
        if actual != set(expected):
            raise ValueError(
                "release-profile saved-case inventory changed; "
                f"missing={sorted(set(expected) - actual)}, "
                f"unexpected={sorted(actual - set(expected))}"
            )
        ignored = set(expected) & list_case_tests("--ignored")
        if ignored:
            raise ValueError(
                f"required saved-case contracts may not be ignored: {sorted(ignored)}"
            )

        binary_provenance = receipt_provenance() if output else None
        print(
            f"Verified {len(expected)} release-profile saved-case contracts", flush=True
        )
        # Run the entire target once, including the existing tunnel evidence test.
        completed = subprocess.run(
            [*CARGO_TEST, "--", "--nocapture"],
            cwd=ROOT,
            check=False,
            timeout=TIMEOUT_SECONDS,
        )
        if completed.returncode:
            return completed.returncode
        if output and binary_provenance is not None:
            binary, provenance = binary_provenance
            if binary_digest(binary) != provenance["binary_sha256"]:
                raise ValueError(
                    "conformance binary changed during execution; refusing a receipt"
                )
            receipt = {
                "$schema": "hooklistener.cases.cli-platform-evidence/1",
                "schema_version": 1,
                "status": "passed",
                "backend": "local_mock_http",
                "live_backend_verified": False,
                "contract_schema_major": 1,
                "test_count": len(expected),
                "tests": expected,
                "provenance": provenance,
            }
            output.parent.mkdir(parents=True, exist_ok=True)
            output.write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
        return 0
    except subprocess.CalledProcessError as error:
        if error.stderr:
            sys.stderr.write(error.stderr)
        print(
            f"Saved-case gate command failed (exit {error.returncode})", file=sys.stderr
        )
        return error.returncode
    except (OSError, ValueError, subprocess.TimeoutExpired) as error:
        print(f"Saved-case conformance failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
