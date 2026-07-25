#!/usr/bin/env python3

from pathlib import Path
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
EXPECTED_INVENTORY = ROOT / "fixtures/tunnel_v3_release_test_inventory.txt"
CARGO_TEST = [
    "cargo",
    "test",
    "--release",
    "--locked",
    "--bin",
    "hooklistener",
]


def main() -> int:
    expected_lines = EXPECTED_INVENTORY.read_text(
        encoding="utf-8"
    ).splitlines()
    if (
        not expected_lines
        or any(not line for line in expected_lines)
        or len(expected_lines) != len(set(expected_lines))
        or expected_lines != sorted(expected_lines)
    ):
        print(
            "release-profile V3 test inventory must be nonempty, unique, and sorted",
            file=sys.stderr,
        )
        return 1

    listing = subprocess.run(
        [*CARGO_TEST, "--", "--list"],
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
    )
    sys.stderr.write(listing.stderr)
    if listing.returncode != 0:
        sys.stdout.write(listing.stdout)
        return listing.returncode

    discovered = {
        line.removesuffix(": test")
        for line in listing.stdout.splitlines()
        if line.endswith(": test")
    }
    actual = {
        name
        for name in discovered
        if name.startswith("tunnel_v3::tests::")
        or name.startswith("tunnel::tests::protocol_v3_")
        or name
        == (
            "tunnel::tests::"
            "tunnel_protocol_selection_prefers_v3_and_defaults_old_services_to_v2"
        )
    }
    expected = set(expected_lines)
    if actual != expected:
        print(
            "release-profile V3 test inventory changed; "
            f"missing={sorted(expected - actual)}, "
            f"unexpected={sorted(actual - expected)}",
            file=sys.stderr,
        )
        return 1

    ignored_listing = subprocess.run(
        [*CARGO_TEST, "--", "--ignored", "--list"],
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
    )
    sys.stderr.write(ignored_listing.stderr)
    if ignored_listing.returncode != 0:
        sys.stdout.write(ignored_listing.stdout)
        return ignored_listing.returncode
    ignored = {
        line.removesuffix(": test")
        for line in ignored_listing.stdout.splitlines()
        if line.endswith(": test")
    }
    ignored_required = sorted(expected & ignored)
    if ignored_required:
        print(
            "release-profile V3 contracts may not be ignored: "
            f"{ignored_required}",
            file=sys.stderr,
        )
        return 1

    print(
        f"Verified {len(actual)} release-profile V3 unit contracts",
        flush=True,
    )
    completed = subprocess.run(CARGO_TEST, cwd=ROOT, check=False)
    return completed.returncode


if __name__ == "__main__":
    raise SystemExit(main())
