#!/usr/bin/env python3
"""Fast measurement-tool tests; no Rust compilation or network access."""

import contextlib
import io
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import measure_build


class MeasurementTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name) / "repo"
        self.root.mkdir()
        (self.root / "Cargo.toml").write_text("manifest", encoding="utf-8")
        (self.root / "Cargo.lock").write_text("lock", encoding="utf-8")
        (self.root / "src").mkdir()
        (self.root / "src/main.rs").write_text("fn main() {}\n", encoding="utf-8")
        (self.root / "fixtures").mkdir()
        (self.root / "fixtures/request.json").write_text("{}", encoding="utf-8")
        self.output = self.root / "target/measurement"

    def test_snapshot_copies_working_sources_without_modifying_checkout(self):
        snapshot = self.root / "copy"
        digest = measure_build.snapshot_source(self.root, snapshot)
        self.assertEqual(len(digest), 64)
        (snapshot / "src/main.rs").write_text("changed", encoding="utf-8")
        self.assertEqual((self.root / "src/main.rs").read_text(), "fn main() {}\n")
        (self.root / "src/main.rs").write_text("working edit", encoding="utf-8")
        other = self.root / "other"
        self.assertNotEqual(digest, measure_build.snapshot_source(self.root, other))
        self.assertEqual((other / "src/main.rs").read_text(), "working edit")

    def test_archive_is_deterministic_and_contains_only_the_executable(self):
        binary = self.root / "hooklistener"
        binary.write_bytes(b"binary contents")
        first, second = self.root / "a.tar.gz", self.root / "b.tar.gz"
        size = measure_build.archive_binary(binary, first)
        measure_build.archive_binary(binary, second)
        self.assertEqual(size, first.stat().st_size)
        self.assertEqual(first.read_bytes(), second.read_bytes())
        with tarfile.open(first) as archive:
            self.assertEqual(archive.getnames(), ["hooklistener"])
            content = archive.extractfile("hooklistener")
            assert content is not None
            self.assertEqual(content.read(), b"binary contents")
            self.assertEqual(archive.getmember("hooklistener").mode, 0o755)

    def test_existing_output_is_never_reused_or_deleted(self):
        self.output.mkdir(parents=True)
        sentinel = self.output / "keep"
        sentinel.write_text("do not delete", encoding="utf-8")
        with self.assertRaises(FileExistsError):
            measure_build.measure(self.root, self.output, "release", 2)
        self.assertEqual(sentinel.read_text(), "do not delete")

    def test_output_inside_source_is_rejected_before_copying(self):
        with self.assertRaisesRegex(ValueError, "outside"):
            measure_build.measure(self.root, self.root / "src/result", "release", 2)
        self.assertFalse((self.root / "src/result").exists())

    def test_failed_build_keeps_log_and_fails_loudly(self):
        log = self.root / "failed.log"
        with patch.object(measure_build.subprocess, "run",
                          return_value=subprocess.CompletedProcess([], 17)):
            with self.assertRaisesRegex(RuntimeError, r"Build failed \(17\)"):
                measure_build.timed_build(["cargo", "build"], self.root, log)
        self.assertTrue(log.exists())

    def test_measurement_records_three_distinct_stages_in_an_isolated_copy(self):
        for profile, directory in (("release", "release"), ("dev", "debug")):
            with self.subTest(profile=profile):
                output = self.output / profile
                stages = []

                def fake_build(command, source, log):
                    self.assertIn("--locked", command)
                    self.assertIn("--offline", command)
                    self.assertEqual(command[command.index("--profile") + 1], profile)
                    stages.append((log.stem, (source / "src/main.rs").read_text()))
                    binary = output / "target/test-host" / directory / "hooklistener"
                    binary.parent.mkdir(parents=True, exist_ok=True)
                    binary.write_bytes(b"fake executable")
                    return 1.0

                with patch.object(measure_build, "timed_build", side_effect=fake_build):
                    with patch.object(measure_build.subprocess, "check_output", side_effect=[
                        "rustc 1.92.0\nhost: test-host", "cargo 1.92.0", "hooklistener 1.8.6"
                    ]), patch.object(measure_build.subprocess, "run"), patch.object(
                        measure_build.platform, "platform", return_value="test-system"
                    ):
                        with contextlib.redirect_stdout(io.StringIO()):
                            measure_build.measure(self.root, output, profile, 2)
                self.assertEqual([stage for stage, _ in stages],
                                 ["clean", "noop", "source_rebuild"])
                self.assertEqual(stages[0][1], stages[1][1])
                self.assertIn("rebuild probe", stages[2][1])
                self.assertNotIn("rebuild probe", (self.root / "src/main.rs").read_text())
                try:
                    report = json.loads((output / "report.json").read_text())
                except (OSError, ValueError) as error:
                    self.fail(f"Measurement did not produce a valid report: {error}")
                self.assertEqual(report["seconds"],
                                 {"clean": 1.0, "noop": 1.0, "source_rebuild": 1.0})
                self.assertEqual(report["binary_bytes"], len(b"fake executable"))
                self.assertEqual(report["version"], "hooklistener 1.8.6")


if __name__ == "__main__":
    unittest.main()
