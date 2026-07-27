#!/usr/bin/env python3

import base64
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[1]
GOVERNANCE_SCRIPT = ROOT / ".github/scripts/verify-release-governance.sh"
MONOTONIC_SCRIPT = ROOT / ".github/scripts/verify-monotonic-release-order.sh"
REMOTE_TAG_SCRIPT = ROOT / ".github/scripts/verify-remote-release-tag.sh"
CI_WORKFLOW = ROOT / ".github/workflows/ci.yml"
RELEASE_WORKFLOW = ROOT / ".github/workflows/release.yml"
CONFORMANCE_WORKFLOW = ROOT / ".github/workflows/tunnel-phase1-conformance.yml"
HOMEBREW_RENDERER = ROOT / ".github/scripts/render-homebrew-formula.py"
V3_TEST_INVENTORY = ROOT / "fixtures/tunnel_v3_release_test_inventory.txt"
V3_TEST_GATE = ROOT / "scripts/verify_tunnel_v3_release_tests.py"
NPM_PUBLISH_SCRIPT = ROOT / "npm/scripts/publish-npm.sh"
NPM_VERIFY_SCRIPT = ROOT / "npm/scripts/verify-package.js"

EXPECTED_CHECKS = [
    "Rustfmt",
    "Clippy",
    "Tests (stable)",
    "Cargo Audit",
    "Analyze",
    "Build (x86_64-unknown-linux-gnu)",
    "Build (x86_64-pc-windows-msvc)",
    "Build (x86_64-apple-darwin)",
    "Build (aarch64-apple-darwin)",
    "Authenticated lifecycle (linux)",
    "Authenticated lifecycle (macos)",
    "Authenticated lifecycle (windows)",
]


def source(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def job_body(workflow: str, job_name: str) -> str:
    match = re.search(
        rf"^  {re.escape(job_name)}:\n(.*?)(?=^  [a-z0-9_-]+:\n|\Z)",
        workflow,
        flags=re.MULTILINE | re.DOTALL,
    )
    if match is None:
        raise AssertionError(f"missing workflow job: {job_name}")
    return match.group(1)


def policy_fixture() -> dict:
    checks = [
        {"context": context, "integration_id": 15368}
        for context in EXPECTED_CHECKS
    ]
    return {
        "main_rules": [
            [
                {"type": "deletion"},
                {"type": "non_fast_forward"},
                {
                    "type": "pull_request",
                    "parameters": {"required_approving_review_count": 1},
                },
                {
                    "type": "required_status_checks",
                    "parameters": {
                        "strict_required_status_checks_policy": True,
                        "required_status_checks": checks,
                    },
                },
            ]
        ],
        "tag_rulesets": [
            {
                "id": 101,
                "target": "tag",
                "enforcement": "active",
                "conditions": {
                    "ref_name": {
                        "include": ["refs/tags/v*.*.*"],
                        "exclude": [],
                    }
                },
                "rules": [{"type": "creation"}],
            },
            {
                "id": 102,
                "target": "tag",
                "enforcement": "active",
                "conditions": {
                    "ref_name": {
                        "include": ["refs/tags/v*.*.*"],
                        "exclude": [],
                    }
                },
                "rules": [{"type": "update"}, {"type": "deletion"}],
            },
        ],
        "environment": {
            "name": "release",
            "can_admins_bypass": False,
            "protection_rules": [
                {
                    "type": "required_reviewers",
                    "prevent_self_review": True,
                    "reviewers": [{"type": "User", "reviewer": {"id": 1}}],
                }
            ],
            "deployment_branch_policy": {
                "protected_branches": False,
                "custom_branch_policies": True,
            },
        },
        "environment_policies": [
            {
                "total_count": 1,
                "branch_policies": [{"id": 1, "name": "v*.*.*"}],
            }
        ],
    }


class GovernancePolicyTest(unittest.TestCase):
    def run_policy(self, fixture: dict) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as temp_dir:
            temp = Path(temp_dir)
            fixture_dir = temp / "fixtures"
            bin_dir = temp / "bin"
            fixture_dir.mkdir()
            bin_dir.mkdir()

            (fixture_dir / "main-rules.json").write_text(
                json.dumps(fixture["main_rules"]),
                encoding="utf-8",
            )
            summaries = [
                {
                    "id": ruleset["id"],
                    "target": ruleset["target"],
                    "enforcement": ruleset["enforcement"],
                }
                for ruleset in fixture["tag_rulesets"]
            ]
            (fixture_dir / "tag-rulesets.json").write_text(
                json.dumps([summaries]),
                encoding="utf-8",
            )
            for ruleset in fixture["tag_rulesets"]:
                (fixture_dir / f"ruleset-{ruleset['id']}.json").write_text(
                    json.dumps(ruleset),
                    encoding="utf-8",
                )
            (fixture_dir / "environment.json").write_text(
                json.dumps(fixture["environment"]),
                encoding="utf-8",
            )
            (fixture_dir / "environment-policies.json").write_text(
                json.dumps(fixture["environment_policies"]),
                encoding="utf-8",
            )

            fake_gh = bin_dir / "gh"
            fake_gh.write_text(
                textwrap.dedent(
                    """\
                    #!/usr/bin/env python3
                    import os
                    from pathlib import Path
                    import re
                    import sys

                    endpoint = next(
                        (
                            argument
                            for argument in sys.argv[1:]
                            if argument.startswith("repos/")
                        ),
                        None,
                    )
                    if endpoint is None:
                        raise SystemExit("fake gh received no API endpoint")

                    fixtures = Path(os.environ["HOOKLISTENER_POLICY_FIXTURES"])
                    if "/rules/branches/main?" in endpoint:
                        name = "main-rules.json"
                    elif "/rulesets?targets=tag" in endpoint:
                        name = "tag-rulesets.json"
                    elif match := re.search(r"/rulesets/([0-9]+)$", endpoint):
                        name = f"ruleset-{match.group(1)}.json"
                    elif endpoint.endswith(
                        "/environments/release/deployment-branch-policies"
                        "?per_page=100"
                    ):
                        name = "environment-policies.json"
                    elif endpoint.endswith("/environments/release"):
                        name = "environment.json"
                    else:
                        raise SystemExit(f"unexpected fake gh endpoint: {endpoint}")

                    sys.stdout.write((fixtures / name).read_text(encoding="utf-8"))
                    """
                ),
                encoding="utf-8",
            )
            fake_gh.chmod(0o755)

            env = os.environ.copy()
            env.update(
                {
                    "GH_TOKEN": "fixture-token",
                    "GITHUB_REPOSITORY": "hooklistener/hooklistener-cli",
                    "HOOKLISTENER_POLICY_FIXTURES": str(fixture_dir),
                    "PATH": f"{bin_dir}{os.pathsep}{env['PATH']}",
                    "RUNNER_TEMP": str(temp),
                }
            )
            return subprocess.run(
                ["bash", str(GOVERNANCE_SCRIPT)],
                cwd=ROOT,
                env=env,
                check=False,
                capture_output=True,
                text=True,
            )

    def assert_policy_passes(self, fixture: dict) -> None:
        result = self.run_policy(fixture)
        self.assertEqual(
            result.returncode,
            0,
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}",
        )

    def assert_policy_fails(self, fixture: dict) -> None:
        result = self.run_policy(fixture)
        self.assertNotEqual(
            result.returncode,
            0,
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}",
        )

    def test_complete_policy_passes(self) -> None:
        self.assert_policy_passes(policy_fixture())

    def test_layered_strictness_and_checks_pass(self) -> None:
        fixture = policy_fixture()
        rules = fixture["main_rules"][0]
        status_rule = rules.pop()
        checks = status_rule["parameters"]["required_status_checks"]
        rules.extend(
            [
                {
                    "type": "required_status_checks",
                    "parameters": {
                        "strict_required_status_checks_policy": True,
                        "required_status_checks": [],
                    },
                },
                {
                    "type": "required_status_checks",
                    "parameters": {
                        "strict_required_status_checks_policy": False,
                        "required_status_checks": checks[:6],
                    },
                },
            ]
        )
        fixture["main_rules"].append(
            [
                {
                    "type": "required_status_checks",
                    "parameters": {
                        "strict_required_status_checks_policy": False,
                        "required_status_checks": checks[6:],
                    },
                }
            ]
        )
        self.assert_policy_passes(fixture)

    def test_missing_or_unpinned_check_fails(self) -> None:
        for mutation in ("missing", "wrong-app"):
            fixture = policy_fixture()
            checks = fixture["main_rules"][0][-1]["parameters"][
                "required_status_checks"
            ]
            if mutation == "missing":
                checks.pop()
            else:
                checks[0]["integration_id"] = None
            with self.subTest(mutation=mutation):
                self.assert_policy_fails(fixture)

    def test_tag_ruleset_exclusion_fails_closed(self) -> None:
        for index in (0, 1):
            fixture = policy_fixture()
            fixture["tag_rulesets"][index]["conditions"]["ref_name"][
                "exclude"
            ] = ["refs/tags/v1.*.*"]
            with self.subTest(ruleset=index):
                self.assert_policy_fails(fixture)

    def test_creation_and_immutability_must_be_separate(self) -> None:
        fixture = policy_fixture()
        combined = fixture["tag_rulesets"][0]
        combined["rules"] = [
            {"type": "creation"},
            {"type": "update"},
            {"type": "deletion"},
        ]
        fixture["tag_rulesets"] = [combined]
        self.assert_policy_fails(fixture)

    def test_release_environment_policy_is_exact(self) -> None:
        fixture = policy_fixture()
        fixture["environment_policies"][0]["branch_policies"].append(
            {"id": 2, "name": "main"}
        )
        self.assert_policy_fails(fixture)


class MonotonicReleaseOrderTest(unittest.TestCase):
    def run_order(
        self,
        *,
        releases: list[dict],
        crates_versions: list[str],
        npm_versions: list[str],
        yanked_crates: set[str] | None = None,
        stage: str = "verify",
        tag: str = "v1.8.0",
    ) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as temp_dir:
            temp = Path(temp_dir)
            fixture_dir = temp / "fixtures"
            bin_dir = temp / "bin"
            fixture_dir.mkdir()
            bin_dir.mkdir()

            (fixture_dir / "releases.json").write_text(
                json.dumps([releases]),
                encoding="utf-8",
            )
            (fixture_dir / "crates.json").write_text(
                json.dumps(
                    {
                        "versions": [
                            {
                                "num": version,
                                "yanked": version
                                in (yanked_crates or set()),
                            }
                            for version in crates_versions
                        ]
                    }
                ),
                encoding="utf-8",
            )
            (fixture_dir / "npm.json").write_text(
                json.dumps(
                    {
                        "dist-tags": {"latest": npm_versions[0]},
                        "versions": {version: {} for version in npm_versions},
                    }
                ),
                encoding="utf-8",
            )

            fake_gh = bin_dir / "gh"
            fake_gh.write_text(
                textwrap.dedent(
                    """\
                    #!/usr/bin/env python3
                    import os
                    from pathlib import Path
                    import sys

                    endpoint = next(
                        (
                            argument
                            for argument in sys.argv[1:]
                            if argument.startswith("repos/")
                        ),
                        None,
                    )
                    if endpoint is None or "/releases?per_page=100" not in endpoint:
                        raise SystemExit(f"unexpected fake gh endpoint: {endpoint}")
                    fixtures = Path(os.environ["HOOKLISTENER_ORDER_FIXTURES"])
                    sys.stdout.write(
                        (fixtures / "releases.json").read_text(encoding="utf-8")
                    )
                    """
                ),
                encoding="utf-8",
            )
            fake_gh.chmod(0o755)

            fake_curl = bin_dir / "curl"
            fake_curl.write_text(
                textwrap.dedent(
                    """\
                    #!/usr/bin/env python3
                    import os
                    from pathlib import Path
                    import sys

                    arguments = sys.argv[1:]
                    output = Path(arguments[arguments.index("--output") + 1])
                    url = next(
                        argument
                        for argument in arguments
                        if argument.startswith("https://")
                    )
                    fixtures = Path(os.environ["HOOKLISTENER_ORDER_FIXTURES"])
                    if "crates.io" in url:
                        fixture = fixtures / "crates.json"
                    elif "registry.npmjs.org" in url:
                        fixture = fixtures / "npm.json"
                    else:
                        raise SystemExit(f"unexpected fake curl URL: {url}")
                    output.write_bytes(fixture.read_bytes())
                    sys.stdout.write("200")
                    """
                ),
                encoding="utf-8",
            )
            fake_curl.chmod(0o755)

            env = os.environ.copy()
            env.update(
                {
                    "GH_TOKEN": "fixture-token",
                    "GITHUB_REPOSITORY": "hooklistener/hooklistener-cli",
                    "GITHUB_SERVER_URL": "https://github.com",
                    "HOOKLISTENER_ORDER_FIXTURES": str(fixture_dir),
                    "PATH": f"{bin_dir}{os.pathsep}{env['PATH']}",
                    "RUNNER_TEMP": str(temp),
                }
            )
            return subprocess.run(
                ["bash", str(MONOTONIC_SCRIPT), tag, stage],
                cwd=ROOT,
                env=env,
                check=False,
                capture_output=True,
                text=True,
            )

    @staticmethod
    def release(
        version: str,
        *,
        prerelease: bool = False,
    ) -> dict:
        return {
            "tag_name": f"v{version}",
            "draft": False,
            "prerelease": prerelease,
        }

    def assert_order_passes(
        self,
        result: subprocess.CompletedProcess[str],
        decision: str,
    ) -> None:
        self.assertEqual(
            result.returncode,
            0,
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}",
        )
        self.assertEqual(result.stdout.strip(), decision)

    def assert_order_fails(
        self,
        result: subprocess.CompletedProcess[str],
    ) -> None:
        self.assertNotEqual(
            result.returncode,
            0,
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}",
        )

    def test_new_release_and_exact_prerelease_resume_pass(self) -> None:
        self.assert_order_passes(
            self.run_order(
                releases=[self.release("1.7.3")],
                crates_versions=["1.7.3"],
                npm_versions=["1.7.3"],
            ),
            "proceed",
        )
        self.assert_order_passes(
            self.run_order(
                releases=[
                    self.release("1.8.0", prerelease=True),
                    self.release("1.7.3"),
                ],
                crates_versions=["1.8.0", "1.7.3"],
                npm_versions=["1.7.3", "1.8.0"],
                stage="npm-publish",
            ),
            "proceed",
        )

    def test_true_maximum_from_every_public_channel_blocks_downgrade(self) -> None:
        fixtures = {
            "GitHub": {
                "releases": [
                    self.release("1.9.0", prerelease=True),
                    self.release("1.7.3"),
                ],
                "crates_versions": ["1.7.3"],
                "npm_versions": ["1.7.3"],
            },
            "crates.io": {
                "releases": [self.release("1.7.3")],
                "crates_versions": ["1.9.0", "1.7.3"],
                "npm_versions": ["1.7.3"],
            },
            "npm-hidden-by-latest-tag": {
                "releases": [self.release("1.7.3")],
                "crates_versions": ["1.7.3"],
                "npm_versions": ["1.7.3", "1.9.0"],
            },
        }
        for source, fixture in fixtures.items():
            with self.subTest(source=source):
                self.assert_order_fails(self.run_order(**fixture))

    def test_registry_equality_without_prerelease_fails(self) -> None:
        self.assert_order_fails(
            self.run_order(
                releases=[self.release("1.7.3")],
                crates_versions=["1.8.0", "1.7.3"],
                npm_versions=["1.7.3"],
            )
        )

    def test_promotion_is_idempotent_without_downgrading_latest(self) -> None:
        self.assert_order_passes(
            self.run_order(
                releases=[
                    self.release("1.8.0"),
                    self.release("1.7.3"),
                ],
                crates_versions=["1.8.0", "1.7.3"],
                npm_versions=["1.8.0", "1.7.3"],
                stage="promote",
            ),
            "promote",
        )
        self.assert_order_passes(
            self.run_order(
                releases=[
                    self.release("1.9.0"),
                    self.release("1.8.0"),
                ],
                crates_versions=["1.9.0", "1.8.0"],
                npm_versions=["1.9.0", "1.8.0"],
                stage="promote",
            ),
            "superseded",
        )
        self.assert_order_fails(
            self.run_order(
                releases=[
                    self.release("1.9.0"),
                    self.release("1.8.0", prerelease=True),
                ],
                crates_versions=["1.9.0", "1.8.0"],
                npm_versions=["1.9.0", "1.8.0"],
                stage="promote",
            )
        )

    def test_homebrew_and_promotion_require_both_exact_registries(self) -> None:
        releases = [
            self.release("1.8.0", prerelease=True),
            self.release("1.7.3"),
        ]
        self.assert_order_passes(
            self.run_order(
                releases=releases,
                crates_versions=["1.8.0", "1.7.3"],
                npm_versions=["1.8.0", "1.7.3"],
                stage="homebrew-push",
            ),
            "proceed",
        )
        self.assert_order_passes(
            self.run_order(
                releases=releases,
                crates_versions=["1.8.0", "1.7.3"],
                npm_versions=["1.8.0", "1.7.3"],
                stage="promote",
            ),
            "promote",
        )

        missing_cases = {
            "crates": {
                "crates_versions": ["1.7.3"],
                "npm_versions": ["1.8.0", "1.7.3"],
            },
            "npm": {
                "crates_versions": ["1.8.0", "1.7.3"],
                "npm_versions": ["1.7.3"],
            },
            "yanked-crate": {
                "crates_versions": ["1.8.0", "1.7.3"],
                "npm_versions": ["1.8.0", "1.7.3"],
                "yanked_crates": {"1.8.0"},
            },
        }
        for stage in ("homebrew-push", "promote"):
            for missing, fixture in missing_cases.items():
                with self.subTest(stage=stage, missing=missing):
                    self.assert_order_fails(
                        self.run_order(
                            releases=releases,
                            stage=stage,
                            **fixture,
                        )
                    )

        for prerelease in (True, False):
            with self.subTest(stage="promote", missing="both", prerelease=prerelease):
                self.assert_order_fails(
                    self.run_order(
                        releases=[
                            self.release("1.8.0", prerelease=prerelease),
                            self.release("1.7.3"),
                        ],
                        crates_versions=["1.7.3"],
                        npm_versions=["1.7.3"],
                        stage="promote",
                    )
                )


class AnnotatedReleaseTagTest(unittest.TestCase):
    def run_remote_tag_check(
        self,
        remote_refs: str,
        source_sha: str,
    ) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as temp_dir:
            temp = Path(temp_dir)
            bin_dir = temp / "bin"
            bin_dir.mkdir()
            fake_git = bin_dir / "git"
            fake_git.write_text(
                textwrap.dedent(
                    """\
                    #!/usr/bin/env python3
                    import os
                    import sys

                    if sys.argv[1:3] != ["ls-remote", "--exit-code"]:
                        raise SystemExit(f"unexpected fake git command: {sys.argv[1:]}")
                    sys.stdout.write(os.environ["HOOKLISTENER_REMOTE_REFS"])
                    """
                ),
                encoding="utf-8",
            )
            fake_git.chmod(0o755)
            env = os.environ.copy()
            env.update(
                {
                    "HOOKLISTENER_REMOTE_REFS": remote_refs,
                    "PATH": f"{bin_dir}{os.pathsep}{env['PATH']}",
                }
            )
            return subprocess.run(
                ["bash", str(REMOTE_TAG_SCRIPT), "v1.8.0", source_sha],
                cwd=ROOT,
                env=env,
                check=False,
                capture_output=True,
                text=True,
            )

    def test_only_annotated_exact_sha_tag_passes(self) -> None:
        source_sha = "a" * 40
        tag_object_sha = "b" * 40
        annotated = (
            f"{tag_object_sha}\trefs/tags/v1.8.0\n"
            f"{source_sha}\trefs/tags/v1.8.0^{{}}\n"
        )
        lightweight = f"{source_sha}\trefs/tags/v1.8.0\n"
        wrong_peeled = (
            f"{tag_object_sha}\trefs/tags/v1.8.0\n"
            f"{'c' * 40}\trefs/tags/v1.8.0^{{}}\n"
        )

        self.assertEqual(
            self.run_remote_tag_check(annotated, source_sha).returncode,
            0,
        )
        self.assertNotEqual(
            self.run_remote_tag_check(lightweight, source_sha).returncode,
            0,
        )
        self.assertNotEqual(
            self.run_remote_tag_check(wrong_peeled, source_sha).returncode,
            0,
        )


class NpmPublishArtifactTest(unittest.TestCase):
    def test_publish_uses_the_exact_checked_tarball_without_source_rewrite(
        self,
    ) -> None:
        package_json = ROOT / "npm/packages/hooklistener/package.json"
        package_before = package_json.read_bytes()
        version = json.loads(package_before)["version"]

        with tempfile.TemporaryDirectory() as temp_dir:
            temp = Path(temp_dir)
            bin_dir = temp / "bin"
            bin_dir.mkdir()
            tarball = temp / f"hooklistener-{version}.tgz"
            tarball.write_bytes(b"exact npm package bytes")
            integrity = "sha512-" + base64.b64encode(
                hashlib.sha512(tarball.read_bytes()).digest()
            ).decode("ascii")
            invocation = temp / "npm-invocation.json"

            fake_npm = bin_dir / "npm"
            fake_npm.write_text(
                textwrap.dedent(
                    """\
                    #!/usr/bin/env python3
                    import json
                    import os
                    from pathlib import Path
                    import sys

                    Path(os.environ["HOOKLISTENER_NPM_INVOCATION"]).write_text(
                        json.dumps(sys.argv[1:]),
                        encoding="utf-8",
                    )
                    """
                ),
                encoding="utf-8",
            )
            fake_npm.chmod(0o755)

            env = os.environ.copy()
            env.update(
                {
                    "HOOKLISTENER_NPM_INVOCATION": str(invocation),
                    "NODE_AUTH_TOKEN": "fixture-token",
                    "PATH": f"{bin_dir}{os.pathsep}{env['PATH']}",
                }
            )
            result = subprocess.run(
                [
                    "bash",
                    str(NPM_PUBLISH_SCRIPT),
                    version,
                    str(tarball),
                    integrity,
                ],
                cwd=ROOT,
                env=env,
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(
                result.returncode,
                0,
                f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}",
            )
            self.assertEqual(package_json.read_bytes(), package_before)
            self.assertEqual(
                json.loads(invocation.read_text(encoding="utf-8")),
                [
                    "publish",
                    str(tarball),
                    "--access",
                    "public",
                    "--ignore-scripts",
                ],
            )

            invocation.unlink()
            rejected = subprocess.run(
                [
                    "bash",
                    str(NPM_PUBLISH_SCRIPT),
                    version,
                    str(tarball),
                    "sha512-" + "A" * 88,
                ],
                cwd=ROOT,
                env=env,
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(rejected.returncode, 0)
            self.assertFalse(invocation.exists())
            self.assertEqual(package_json.read_bytes(), package_before)


class ReleaseWorkflowTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.ci = source(CI_WORKFLOW)
        cls.release = source(RELEASE_WORKFLOW)
        cls.conformance = source(CONFORMANCE_WORKFLOW)

    def test_publication_waits_for_exact_cross_platform_conformance(self) -> None:
        release = job_body(self.release, "release")
        self.assertIn(
            "needs: [verify, tunnel-v3-conformance, build]",
            release,
        )
        self.assertIn("environment: release", release)

        called = job_body(self.release, "tunnel-v3-conformance")
        self.assertIn(
            "uses: ./.github/workflows/tunnel-phase1-conformance.yml",
            called,
        )
        self.assertIn("source_sha: ${{ github.sha }}", called)
        self.assertIn("source_ref: ${{ github.ref }}", called)

        self.assertIn("platform: linux", self.conformance)
        self.assertIn("platform: macos", self.conformance)
        self.assertIn("platform: windows", self.conformance)
        self.assertIn("ref: ${{ inputs.source_sha || github.sha }}", self.conformance)
        self.assertIn("pattern: dist-*", release)

    def test_source_verification_supports_macos_bash_3(self) -> None:
        for workflow in (self.conformance, self.release):
            with self.subTest(workflow=workflow[:20]):
                self.assertNotRegex(workflow, r"\$\{[^}]+,,\}")
                self.assertIn("tr '[:upper:]' '[:lower:]'", workflow)

    def test_cargo_audit_is_pinned_and_audits_the_committed_lockfile(self) -> None:
        install = "cargo install cargo-audit --version 0.22.2 --locked"
        audit = "cargo audit --file Cargo.lock"

        for workflow, job_name in ((self.ci, "audit"), (self.release, "verify")):
            body = job_body(workflow, job_name)
            with self.subTest(job=job_name):
                self.assertIn(install, body)
                self.assertIn(audit, body)
                self.assertLess(body.find(install), body.find(audit))
                # rustsec/audit-check regenerates Cargo.lock before auditing, so
                # it audits dependencies the commit never locked and leaves the
                # working tree dirty for `cargo publish --dry-run --locked`.
                executable = "\n".join(
                    line
                    for line in body.splitlines()
                    if not line.lstrip().startswith("#")
                )
                self.assertNotIn("rustsec/audit-check@", executable)
                self.assertNotIn("generate-lockfile", executable)

    def test_release_verification_asserts_a_pristine_tree_before_publishing(
        self,
    ) -> None:
        body = job_body(self.release, "verify")
        cleanliness = body.find("git status --porcelain")
        dry_run = body.find("cargo publish --dry-run --locked")

        self.assertNotEqual(cleanliness, -1)
        self.assertNotEqual(dry_run, -1)
        self.assertLess(cleanliness, dry_run)

    def test_build_caches_are_scoped_to_the_runner_image(self) -> None:
        # CI builds Linux on ubuntu-latest while the release pins ubuntu-22.04
        # for its glibc floor. rust-cache keys on `runner.os` ("Linux" for both)
        # and the job id ("build" in both workflows), so without the image in the
        # key one job restores C artifacts (aws-lc-sys) built against a different
        # glibc and linking fails on undefined __isoc23_* symbols.
        for workflow, label in ((self.ci, "ci"), (self.release, "release")):
            body = job_body(workflow, "build")
            with self.subTest(workflow=label):
                cache = body.find("Swatinem/rust-cache@")
                self.assertNotEqual(cache, -1)
                self.assertIn("key: ${{ matrix.os }}", body[cache:])

        self.assertIn("os: ubuntu-22.04", job_body(self.release, "build"))

    def test_every_release_asset_policy_is_exact_and_unique(self) -> None:
        expected = {
            "SHA256SUMS.txt",
            "hooklistener-aarch64-apple-darwin.tar.gz",
            "hooklistener-x86_64-apple-darwin.tar.gz",
            "hooklistener-x86_64-unknown-linux-gnu.tar.gz",
            "hooklistener.exe-x86_64-pc-windows-msvc.zip",
        }
        arrays = re.findall(
            r"--argjson expected_assets '\[\n(.*?)\n\s*\]'",
            self.release,
            flags=re.DOTALL,
        )
        self.assertGreater(len(arrays), 0)
        for index, array in enumerate(arrays):
            assets = re.findall(r'"([^"]+)"', array)
            with self.subTest(index=index):
                self.assertEqual(len(assets), len(expected))
                self.assertEqual(set(assets), expected)

    def test_public_publisher_jobs_are_transitively_gated(self) -> None:
        expected_needs = {
            "publish-crates": "needs: [verify, release]",
            "publish-npm": "needs: [verify, release]",
            "update-homebrew": "needs: [publish-crates, publish-npm]",
            "promote-release": (
                "needs: [release, publish-crates, publish-npm, update-homebrew]"
            ),
        }
        for job_name, expected in expected_needs.items():
            with self.subTest(job=job_name):
                self.assertIn(expected, job_body(self.release, job_name))

    def test_every_public_mutation_job_rechecks_current_governance(self) -> None:
        first_mutation = {
            "release": 'gh release create "${TAG_NAME}"',
            "publish-crates": "cargo publish --locked",
            "publish-npm": "npm/scripts/publish-npm.sh",
            "update-homebrew": "git push origin HEAD:refs/heads/main",
            "promote-release": "gh release edit",
        }
        for job_name, mutation in first_mutation.items():
            body = job_body(self.release, job_name)
            governance = body.find(
                "bash .github/scripts/verify-release-governance.sh"
            )
            with self.subTest(job=job_name):
                self.assertIn("actions: read", body)
                self.assertGreaterEqual(governance, 0)
                self.assertLess(governance, body.find(mutation))

    def test_remote_tag_is_rechecked_in_each_publisher_shell(self) -> None:
        expected_mutations = {
            "release": [
                'gh release create "${TAG_NAME}"',
                'gh release upload "${TAG_NAME}"',
            ],
            "publish-crates": ["cargo publish --locked"],
            "publish-npm": ["npm/scripts/publish-npm.sh"],
            "update-homebrew": ["git push"],
            "promote-release": ["gh release edit"],
        }
        self.assertNotIn("softprops/action-gh-release", self.release)
        for job_name, mutations in expected_mutations.items():
            body = job_body(self.release, job_name)
            for mutation in mutations:
                mutation_index = body.find(mutation)
                remote_check = body.rfind(
                    "bash .github/scripts/verify-remote-release-tag.sh",
                    0,
                    mutation_index + 1,
                )
                with self.subTest(job=job_name, mutation=mutation):
                    self.assertGreaterEqual(mutation_index, 0)
                    self.assertGreaterEqual(remote_check, 0)
                    between = body[remote_check:mutation_index]
                    self.assertNotIn("\n      - name:", between)

    def test_monotonic_order_is_rechecked_before_every_public_mutation(
        self,
    ) -> None:
        expected_mutations = {
            "release": {
                'gh release create "${TAG_NAME}"': "prerelease-create",
                'gh release upload "${TAG_NAME}"': "prerelease-resume",
            },
            "publish-crates": {"cargo publish --locked": "crates-publish"},
            "publish-npm": {
                "npm/scripts/publish-npm.sh": "npm-publish",
            },
            "update-homebrew": {
                "git push origin HEAD:refs/heads/main": "homebrew-push",
            },
            "promote-release": {"gh release edit": "promote"},
        }
        order_script = (
            "bash .github/scripts/verify-monotonic-release-order.sh"
        )
        remote_script = "bash .github/scripts/verify-remote-release-tag.sh"

        for job_name, mutations in expected_mutations.items():
            body = job_body(self.release, job_name)
            for mutation, stage in mutations.items():
                mutation_index = body.find(mutation)
                order_index = body.rfind(
                    order_script,
                    0,
                    mutation_index + 1,
                )
                remote_index = body.rfind(
                    remote_script,
                    0,
                    mutation_index + 1,
                )
                with self.subTest(job=job_name, mutation=mutation):
                    self.assertGreaterEqual(order_index, 0)
                    self.assertIn(stage, body[order_index:mutation_index])
                    self.assertGreater(remote_index, order_index)
                    self.assertLess(remote_index, mutation_index)
                    self.assertNotIn(
                        "\n      - name:",
                        body[order_index:mutation_index],
                    )

        promotion = job_body(self.release, "promote-release")
        monotonic = promotion.rfind(
            order_script,
            0,
            promotion.find("gh release edit") + 1,
        )
        final_snapshot = promotion.find(
            'release_final="${RUNNER_TEMP}/release-final-pre-promotion.json"',
            monotonic,
        )
        remote = promotion.find(remote_script, final_snapshot)
        mutation = promotion.find("gh release edit", remote)
        self.assertLess(monotonic, final_snapshot)
        self.assertLess(final_snapshot, remote)
        self.assertLess(remote, mutation)

    def test_annotated_tag_and_v3_test_inventory_are_fail_closed(self) -> None:
        verify = job_body(self.release, "verify")
        self.assertIn(
            'git cat-file -t "refs/tags/${TAG_NAME}"',
            verify,
        )
        self.assertIn(
            "verify-monotonic-release-order.sh",
            verify,
        )

        inventory = source(V3_TEST_INVENTORY).splitlines()
        self.assertEqual(len(inventory), 30)
        self.assertEqual(len(set(inventory)), 30)
        self.assertIn(
            "fixtures/tunnel_v3_release_test_inventory.txt",
            source(V3_TEST_GATE),
        )
        self.assertIn('"--ignored", "--list"', source(V3_TEST_GATE))
        gate_command = "python3 scripts/verify_tunnel_v3_release_tests.py"
        self.assertIn(gate_command, self.conformance)
        self.assertIn(
            gate_command,
            source(ROOT / "docs/tunnel-phase1-conformance.md"),
        )

    def test_npm_publishes_the_integrity_checked_tarball(self) -> None:
        body = job_body(self.release, "publish-npm")
        self.assertIn(
            "--pack-destination \"${package_directory}\"",
            body,
        )
        self.assertIn(
            "PACKAGE_TARBALL: ${{ steps.registry-state.outputs.package_path }}",
            body,
        )
        self.assertIn(
            "EXPECTED_INTEGRITY: ${{ steps.registry-state.outputs.integrity }}",
            body,
        )

        publish_script = source(NPM_PUBLISH_SCRIPT)
        self.assertNotIn("package.json.tmp", publish_script)
        self.assertNotIn(".version = $v", publish_script)
        self.assertIn(
            'npm publish "${PACKAGE_TARBALL}"',
            publish_script,
        )

    def test_npm_launcher_mode_is_verified_in_ci_and_before_publish(self) -> None:
        verify_command = "node npm/scripts/verify-package.js"
        self.assertIn(verify_command, job_body(self.ci, "test"))
        self.assertIn(verify_command, job_body(self.release, "verify"))

        publish = job_body(self.release, "publish-npm")
        pack = publish.find("npm pack")
        verify = publish.find(verify_command, pack)
        registry_read = publish.find("registry.npmjs.org", verify)
        self.assertGreaterEqual(pack, 0)
        self.assertGreater(verify, pack)
        self.assertGreater(registry_read, verify)

        verifier = source(NPM_VERIFY_SCRIPT)
        self.assertIn('["bin/hooklistener.js", 0o755]', verifier)

    def test_operator_governance_queries_are_paginated(self) -> None:
        contributing = source(ROOT / "CONTRIBUTING.md")
        self.assertGreaterEqual(
            contributing.count("gh api --paginate --slurp"),
            3,
        )
        self.assertIn(
            "rules/branches/main?per_page=100",
            contributing,
        )

    def test_conformance_receipts_are_attempt_scoped(self) -> None:
        self.assertIn(
            "name: phase1-${{ matrix.platform }}-${{ github.run_id }}-"
            "${{ github.run_attempt }}",
            self.conformance,
        )

    def test_contract_suite_runs_in_ci_and_release_verification(self) -> None:
        command = "python3 scripts/release_workflow_contract_test.py"
        self.assertIn(command, source(ROOT / ".github/workflows/ci.yml"))
        self.assertIn(command, job_body(self.release, "verify"))

    def test_homebrew_formula_is_rendered_and_rechecked_deterministically(
        self,
    ) -> None:
        body = job_body(self.release, "update-homebrew")
        self.assertGreaterEqual(
            body.count("python3 .github/scripts/render-homebrew-formula.py"),
            2,
        )
        self.assertIn("cmp --silent", body)
        self.assertIn("ruby -c homebrew-tap/Formula/hooklistener.rb", body)
        self.assertIn('git diff --cached --name-only -z', body)
        self.assertIn("git diff --cached --check", body)
        self.assertIn("git push origin HEAD:refs/heads/main", body)

        digest = "a" * 64
        result = subprocess.run(
            [
                "python3",
                str(HOMEBREW_RENDERER),
                "--version",
                "1.8.0",
                "--arm64-sha",
                digest,
                "--x86-64-sha",
                digest,
                "--linux-sha",
                digest,
            ],
            check=True,
            capture_output=True,
            text=True,
        )
        self.assertIn('version "1.8.0"', result.stdout)
        self.assertEqual(result.stdout.count(f'sha256 "{digest}"'), 3)
        self.assertIn('shell_output("#{bin}/hooklistener --version")', result.stdout)

        invalid = subprocess.run(
            [
                "python3",
                str(HOMEBREW_RENDERER),
                "--version",
                "1.8.0-rc.1",
                "--arm64-sha",
                digest,
                "--x86-64-sha",
                digest,
                "--linux-sha",
                digest,
            ],
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(invalid.returncode, 0)


if __name__ == "__main__":
    unittest.main()
