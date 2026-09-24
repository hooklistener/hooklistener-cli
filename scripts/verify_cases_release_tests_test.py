#!/usr/bin/env python3
"""Deterministic tests for the gate itself; no cargo, network or backend required."""

import contextlib
import hashlib
import importlib.util
import io
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "verify_cases_release_tests",
    Path(__file__).with_name("verify_cases_release_tests.py"),
)
if spec is None or spec.loader is None:
    raise RuntimeError("Cannot load the saved-case conformance gate")
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


class SavedCaseGateTest(unittest.TestCase):
    def setUp(self) -> None:
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.inventory = self.root / "inventory.txt"
        self.inventory.write_text("cases::alpha\ncases::beta\n", encoding="utf-8")
        self.binary = self.root / "hooklistener"
        self.binary.write_bytes(b"fixture binary")
        self.output = self.root / "receipt.json"
        self.output.write_text('{"status":"passed","stale":true}', encoding="utf-8")
        self.names = ["cases::alpha", "cases::beta", "legacy_tunnel_flow"]
        self.ignored = []
        self.exit_code = 0
        self.dirty = ""
        self.change_binary = False
        self.commands = []
        self.stderr = io.StringIO()
        for patcher in [
            patch.object(gate, "EXPECTED_INVENTORY", self.inventory),
            patch.dict(
                os.environ,
                {
                    "HOOKLISTENER_CASES_CONFORMANCE_OUTPUT": str(self.output),
                    "HOOKLISTENER_CONFORMANCE_BINARY": str(self.binary),
                    "HOOKLISTENER_CLI_GIT_SHA": "a" * 40,
                    "HOOKLISTENER_CONFORMANCE_PLATFORM": "windows",
                    "GITHUB_RUN_ID": "123",
                    "GITHUB_RUN_ATTEMPT": "2",
                },
                clear=True,
            ),
            patch.object(gate.subprocess, "run", side_effect=self.run_command),
        ]:
            patcher.start()
            self.addCleanup(patcher.stop)

    def run_command(self, command, **_kwargs):
        self.commands.append(command)
        if command == ["git", "rev-parse", "HEAD"]:
            return subprocess.CompletedProcess(command, 0, "a" * 40 + "\n", "")
        if command == ["git", "status", "--porcelain"]:
            return subprocess.CompletedProcess(command, 0, self.dirty, "")
        if "--list" in command:
            names = self.ignored if "--ignored" in command else self.names
            stdout = "\n".join(f"{name}: test" for name in names)
            return subprocess.CompletedProcess(command, 0, stdout, "")
        self.assertEqual(command, [*gate.CARGO_TEST, "--", "--nocapture"])
        if self.change_binary:
            self.binary.write_bytes(b"different binary")
        return subprocess.CompletedProcess(command, self.exit_code, "", "")

    def invoke(self) -> int:
        with (
            contextlib.redirect_stdout(io.StringIO()),
            contextlib.redirect_stderr(self.stderr),
        ):
            return gate.main()

    def read_receipt(self) -> dict:
        try:
            return json.loads(self.output.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            self.fail(f"Expected a valid receipt: {error}")

    def assert_no_execution_or_receipt(self) -> None:
        self.assertFalse(self.output.exists())
        self.assertFalse(any("--nocapture" in command for command in self.commands))

    def test_pass_records_mock_scope_source_binary_and_attempt(self) -> None:
        self.assertEqual(self.invoke(), 0)
        receipt = self.read_receipt()
        self.assertEqual(
            receipt["$schema"], "hooklistener.cases.cli-platform-evidence/1"
        )
        self.assertEqual(receipt["status"], "passed")
        self.assertEqual(receipt["backend"], "local_mock_http")
        self.assertFalse(receipt["live_backend_verified"])
        self.assertEqual(receipt["tests"], ["cases::alpha", "cases::beta"])
        self.assertEqual(receipt["test_count"], 2)
        self.assertEqual(
            receipt["provenance"],
            {
                "cli_git_sha": "a" * 40,
                "source_dirty": False,
                "binary_sha256": hashlib.sha256(b"fixture binary").hexdigest(),
                "platform": "windows",
                "run_id": "123",
                "run_attempt": "2",
            },
        )

    def test_runs_whole_target_once_without_skipping_tunnel_tests(self) -> None:
        self.assertEqual(self.invoke(), 0)
        executions = [command for command in self.commands if "--nocapture" in command]
        self.assertEqual(executions, [[*gate.CARGO_TEST, "--", "--nocapture"]])
        for flag in ["--release", "--locked"]:
            self.assertIn(flag, executions[0])

    def test_invalid_inventory_never_invokes_cargo(self) -> None:
        for content in [
            "",
            "\n",
            "cases::alpha\ncases::alpha\n",
            "cases::beta\ncases::alpha\n",
            "cases::alpha\n\ncases::beta\n",
            "other::alpha\n",
            "cases::alpha --ignored\n",
        ]:
            with self.subTest(content=content):
                self.inventory.write_text(content, encoding="utf-8")
                self.assertEqual(self.invoke(), 1)
                self.assertEqual(self.commands, [])
                self.assert_no_execution_or_receipt()

    def test_missing_inventory_fails_closed(self) -> None:
        self.inventory.unlink()
        self.assertEqual(self.invoke(), 1)
        self.assert_no_execution_or_receipt()

    def test_removed_module_is_not_a_zero_test_success(self) -> None:
        self.names = ["legacy_tunnel_flow"]
        self.assertEqual(self.invoke(), 1)
        self.assert_no_execution_or_receipt()

    def test_missing_test_requires_inventory_review(self) -> None:
        self.names = ["cases::alpha"]
        self.assertEqual(self.invoke(), 1)
        self.assert_no_execution_or_receipt()

    def test_added_test_requires_inventory_review(self) -> None:
        self.names.append("cases::gamma")
        self.assertEqual(self.invoke(), 1)
        self.assert_no_execution_or_receipt()

    def test_ignored_required_test_fails_closed(self) -> None:
        self.ignored = ["cases::beta"]
        self.assertEqual(self.invoke(), 1)
        self.assert_no_execution_or_receipt()
        self.assertIn("may not be ignored", self.stderr.getvalue())

    def test_build_failure_removes_stale_success(self) -> None:
        with patch.object(
            gate.subprocess,
            "run",
            side_effect=subprocess.CalledProcessError(
                101, "cargo", stderr="compile failed\n"
            ),
        ):
            self.assertEqual(self.invoke(), 101)
        self.assert_no_execution_or_receipt()

    def test_ignored_listing_failure_does_not_execute_tests(self) -> None:
        original = self.run_command

        def fail_ignored(command, **kwargs):
            if "--ignored" in command:
                raise subprocess.CalledProcessError(
                    101, command, stderr="compile failed\n"
                )
            return original(command, **kwargs)

        with patch.object(gate.subprocess, "run", side_effect=fail_ignored):
            self.assertEqual(self.invoke(), 101)
        self.assert_no_execution_or_receipt()

    def test_test_failure_never_publishes_a_passed_receipt(self) -> None:
        self.exit_code = 101
        self.assertEqual(self.invoke(), 101)
        self.assertFalse(self.output.exists())

    def test_timeout_fails_closed(self) -> None:
        with patch.object(
            gate.subprocess, "run", side_effect=subprocess.TimeoutExpired("cargo", 1200)
        ):
            self.assertEqual(self.invoke(), 1)
        self.assert_no_execution_or_receipt()

    def test_missing_cargo_fails_closed(self) -> None:
        with patch.object(
            gate.subprocess, "run", side_effect=FileNotFoundError("cargo")
        ):
            self.assertEqual(self.invoke(), 1)
        self.assert_no_execution_or_receipt()

    def test_mismatched_source_sha_fails_closed(self) -> None:
        os.environ["HOOKLISTENER_CLI_GIT_SHA"] = "b" * 40
        self.assertEqual(self.invoke(), 1)
        self.assert_no_execution_or_receipt()

    def test_receipt_requires_an_explicit_tested_binary(self) -> None:
        del os.environ["HOOKLISTENER_CONFORMANCE_BINARY"]
        self.assertEqual(self.invoke(), 1)
        self.assert_no_execution_or_receipt()

    def test_binary_changes_during_execution_refuse_evidence(self) -> None:
        self.change_binary = True
        self.assertEqual(self.invoke(), 1)
        self.assertFalse(self.output.exists())

    def test_local_dirty_worktree_is_not_claimed_to_be_pristine(self) -> None:
        self.dirty = " M src/cases.rs\n"
        self.assertEqual(self.invoke(), 0)
        receipt = self.read_receipt()
        self.assertTrue(receipt["provenance"]["source_dirty"])

    def test_local_run_without_receipt_needs_no_binary_override(self) -> None:
        del os.environ["HOOKLISTENER_CASES_CONFORMANCE_OUTPUT"]
        del os.environ["HOOKLISTENER_CONFORMANCE_BINARY"]
        self.assertEqual(self.invoke(), 0)
        self.assertFalse(any(command[0] == "git" for command in self.commands))
        self.assertIn([*gate.CARGO_TEST, "--", "--nocapture"], self.commands)


if __name__ == "__main__":
    unittest.main()
