"""Execute the real CI step bodies and helpers against a recording Nix stub."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

import yaml


ROOT = Path(__file__).resolve().parents[2]
MAIN_EVENT = "github.event_name == 'push' || github.event_name == 'workflow_dispatch'"
TRUSTED_MAIN = (
    "github.repository_owner == 'arcuru' && github.ref == 'refs/heads/main' && "
    f"({MAIN_EVENT})"
)
ZINC = ["self-hosted", "Linux", "ARM64", "zinc", "nix"]
CHECKED = "/nix/store/" + "a" * 32 + "-checked"
ARCHIVE = "/nix/store/" + "b" * 32 + "-test-check-artifacts-archive"
TARGETS = "/nix/store/" + "f" * 32 + "-test-check-artifacts"
LEAVES = [
    ("aarch64-linux", "build", "/nix/store/" + "c" * 32 + "-build.drv"),
    ("aarch64-linux", "test.sqlite", "/nix/store/" + "d" * 32 + "-test-check-sqlite.drv"),
    ("aarch64-linux", "lint.clippy", "/nix/store/" + "e" * 32 + "-clippy.drv"),
]

# Stands in for `nix` and records every invocation. Environment variables pick
# failures; assertions inside it catch credentials reaching evaluation/builds.
STUB = f"""#!{sys.executable}
import json, os, stat, sys
args = sys.argv[1:]
with open(os.environ['NIX_CALLS'], 'a') as f:
    f.write(json.dumps(args) + '\\n')
def code(name):
    return int(os.environ.get(name, '0'))
if args[0] in ('eval', 'build', 'log'):
    assert 'NIX_CACHE_SIGNING_KEY' not in os.environ, 'signing key during ' + args[0]
    assert not any(n.startswith('AWS_') for n in os.environ), 'credentials during ' + args[0]
if args[0] == 'eval':
    if 'constituents' in args[-1]:
        print(os.environ.get('NIX_LEAVES', ''), end='')
        sys.exit(code('LEAVES_EXIT'))
    print(os.environ.get('NIX_NAMES', 'build\\ntest'), end='')
    sys.exit(code('NAMES_EXIT'))
if args[0] == 'build':
    sys.exit(code('BUILD_EXIT'))
if args[0] == 'log':
    print('retained output of ' + args[1])
    sys.exit(code('LOG_EXIT'))
if args[0] == 'path-info':
    if args[1:] == ['.#legacyPackages.x86_64-linux.test.artifacts.out']:
        print({TARGETS!r})
        sys.exit(code('TARGETS_EXIT'))
    if args[1].startswith('.#'):
        print({CHECKED!r})
        print({ARCHIVE!r})
    sys.exit(code('LOCAL_EXIT'))
if args[:2] == ['store', 'sign']:
    key = args[args.index('--key-file') + 1]
    assert stat.S_IMODE(os.stat(key).st_mode) == 0o600
    assert open(key).read() == 'FAKE-TEST-KEY\\n'
    assert 'NIX_CACHE_SIGNING_KEY' not in os.environ
    sys.exit(code('SIGN_EXIT'))
if args[0] == 'copy':
    assert args[1] == '--to' and args[2].startswith('s3://cache-eidetica-dev?')
    sys.exit(code('COPY_EXIT'))
sys.exit(99)
"""


def expression(value):
    return " ".join(value.split())


class Stubbed(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="ci test ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.calls_file = self.root / "calls.jsonl"
        stub = self.root / "nix"
        stub.write_text(STUB)
        stub.chmod(0o755)
        self.env = {
            k: v for k, v in os.environ.items()
            if not k.startswith(("AWS_", "NIX_CACHE_"))
        }
        self.env.update({
            "PATH": f"{self.root}{os.pathsep}{os.environ['PATH']}",
            "RUNNER_TEMP": str(self.root),
            "NIX_CALLS": str(self.calls_file),
            "NIX_LEAVES": "".join(f"{s} {c} {d}\n" for s, c, d in LEAVES),
            "GITHUB_EVENT_NAME": "push",
            "GITHUB_REF": "refs/heads/main",
            "GITHUB_REPOSITORY": "arcuru/eidetica",
            "GITHUB_REPOSITORY_OWNER": "arcuru",
        })

    def run_script(self, script, **changes):
        self.calls_file.unlink(missing_ok=True)
        return subprocess.run(
            [shutil.which("bash"), "-euo", "pipefail", "-c", script],
            cwd=ROOT, env={**self.env, **changes}, capture_output=True,
            text=True, timeout=20,
        )

    def calls(self):
        if not self.calls_file.exists():
            return []
        return [json.loads(line) for line in self.calls_file.read_text().splitlines()]


class Workflow(Stubbed):
    @classmethod
    def setUpClass(cls):
        cls.ci_text = (ROOT / ".github/workflows/ci.yml").read_text()
        # BaseLoader preserves the YAML key `on` and expression strings.
        cls.ci = yaml.load(cls.ci_text, Loader=yaml.BaseLoader)
        cls.jobs = cls.ci["jobs"]

    def step(self, job, name):
        return next(s for s in self.jobs[job]["steps"] if s["name"] == name)

    def run_checks(self, job, event="push", **changes):
        step = self.step(job, "Checks" if job == "ci-arm" else "Checks + integration")
        return self.run_script(step["run"], CI_EVENT_NAME=event, **changes)

    def test_event_and_platform_routing(self):
        self.assertEqual(set(self.ci["on"]), {"push", "pull_request", "workflow_dispatch"})
        self.assertEqual(set(self.jobs), {"ci-arm", "ci", "main", "publish"})
        arm = self.jobs["ci-arm"]
        self.assertEqual(arm["name"], "CI (aarch64-linux)")
        self.assertEqual(arm["runs-on"], ZINC)
        self.assertNotIn("if", arm)
        self.assertNotIn("needs", arm)
        self.assertEqual(arm["timeout-minutes"], "60")
        self.assertEqual(self.jobs["ci"]["name"], "CI (x86_64-linux)")
        self.assertEqual(
            expression(self.jobs["ci"]["if"]),
            f"${{{{ github.event_name != 'pull_request' && !({TRUSTED_MAIN}) }}}}",
        )
        # A folded expression with a trailing newline would never be false.
        self.assertEqual(self.jobs["ci"]["if"], self.jobs["ci"]["if"].strip())
        self.assertNotIn("needs", self.jobs["ci"])
        for job in ("ci", "main"):
            self.assertEqual(self.jobs[job]["runs-on"], "ubicloud-standard-8")
        self.assertEqual(expression(self.jobs["main"]["if"]), TRUSTED_MAIN)
        self.assertEqual(expression(self.jobs["publish"]["if"]), TRUSTED_MAIN)

    def test_arm_targets_per_event(self):
        for event, target in (
            ("pull_request", "legacyPackages.aarch64-linux.ci-pr"),
            ("push", "checks.aarch64-linux"),
            ("workflow_dispatch", "checks.aarch64-linux"),
        ):
            with self.subTest(event=event):
                result = self.run_checks("ci-arm", event)
                self.assertEqual(result.returncode, 0, result.stderr)
                calls = self.calls()
                self.assertEqual(calls[0][:3], ["eval", "--raw", f".#{target}"])
                self.assertEqual(calls[1], [
                    "build", "-L", "--no-link", "--keep-going",
                    f".#{target}.build", f".#{target}.test",
                ])

    def test_x86_jobs_check_and_integrate_in_one_graph(self):
        self.assertEqual(
            self.step("ci", "Checks + integration")["run"],
            self.step("main", "Checks + integration")["run"],
        )
        result = self.run_checks("main", NIX_NAMES="build\ndoc\nintegration\n")
        self.assertEqual(result.returncode, 0, result.stderr)
        target = ".#legacyPackages.x86_64-linux.ci"
        self.assertEqual(self.calls()[1], [
            "build", "-L", "--no-link", "--keep-going",
            f"{target}.build", f"{target}.doc", f"{target}.integration",
        ])

    def test_arm_runs_without_installer_privileges_or_secrets(self):
        steps = self.jobs["ci-arm"]["steps"]
        self.assertEqual([s["name"] for s in steps], ["Checkout", "Checks"])
        for job in self.jobs.values():
            if "steps" in job:
                self.assertEqual(job["steps"][0]["with"], {
                    "ref": "${{ github.sha }}", "persist-credentials": "false",
                })
        arm = yaml.safe_dump(self.jobs["ci-arm"])
        for forbidden in ("secrets.", "sudo", "--option", "environment", "setup-nix"):
            self.assertNotIn(forbidden, arm)
        self.assertNotIn("secrets.", yaml.safe_dump(self.jobs["ci"]))
        self.assertNotIn("environment", self.jobs["ci"])

    def test_build_status_survives_log_replay(self):
        for job in ("ci-arm", "main"):
            for build in (0, 1, 42, 100):
                for log in (0, 17):
                    with self.subTest(job=job, build=build, log=log):
                        result = self.run_checks(
                            job, BUILD_EXIT=str(build), LOG_EXIT=str(log)
                        )
                        self.assertEqual(result.returncode, build, result.stderr)
                        logs = [c for c in self.calls() if c[0] == "log"]
                        self.assertEqual(logs, [["log", d] for _, _, d in LEAVES])
                        for system, check, _ in LEAVES:
                            self.assertIn(
                                f"::group::{system} {check} — replaying stored "
                                "derivation log; check is not being rerun",
                                result.stdout,
                            )
                        self.assertEqual(result.stdout.count("::endgroup::"), len(LEAVES))
                        warnings = result.stdout.count("::warning::")
                        self.assertEqual(warnings, len(LEAVES) if log else 0)
                        if log:
                            self.assertIn("no test result inferred", result.stdout)

    def test_unlisted_logs_warn_without_changing_build_status(self):
        for build in (0, 42):
            for changes in ({"LEAVES_EXIT": "3"}, {"NIX_LEAVES": ""}):
                with self.subTest(build=build, changes=changes):
                    result = self.run_checks("ci-arm", BUILD_EXIT=str(build), **changes)
                    self.assertEqual(result.returncode, build, result.stderr)
                    self.assertFalse(any(c[0] == "log" for c in self.calls()))
                    if "LEAVES_EXIT" in changes:
                        self.assertIn("::warning::Could not list stored check logs", result.stdout)

    def test_unevaluable_or_empty_check_sets_fail_before_building(self):
        for changes, status in (({"NAMES_EXIT": "5"}, 5), ({"NIX_NAMES": ""}, 1)):
            with self.subTest(changes=changes):
                result = self.run_checks("ci-arm", **changes)
                self.assertEqual(result.returncode, status, result.stderr)
                self.assertFalse(any(c[0] == "build" for c in self.calls()))

    def test_main_needs_arm_success_before_any_hosted_step(self):
        main = self.jobs["main"]
        self.assertEqual(main["needs"], ["ci-arm"])
        self.assertEqual(main["environment"], "publish")
        self.assertEqual(main["timeout-minutes"], "60")
        self.assertEqual(self.jobs["publish"]["needs"], ["main"])
        # No always()/status override: a failed, skipped or cancelled ARM job
        # cannot start x86 or publication, and step failures stop the job.
        for job in self.jobs.values():
            self.assertNotIn("continue-on-error", job)
            self.assertNotIn("always()", job.get("if", ""))
            for step in job.get("steps", []):
                self.assertNotIn("continue-on-error", step)
        for step in main["steps"]:
            self.assertNotIn("if", step)
        self.assertEqual([s["name"] for s in main["steps"]], [
            "Checkout", "Setup Nix", "Checks + integration",
            "Prepare checked outputs", "Publish checked outputs",
        ])
        self.assertEqual(self.step("main", "Checks + integration")["timeout-minutes"], "30")

    def test_secrets_reach_only_the_final_publication_step(self):
        main = self.jobs["main"]
        self.assertNotIn("env", main)
        for step in main["steps"][:-1]:
            self.assertNotIn("secrets.", yaml.safe_dump(step))
        self.assertNotIn("with", self.step("main", "Setup Nix"))
        publish = main["steps"][-1]
        self.assertEqual(publish["run"], "bash .github/scripts/publish-cache.sh")
        self.assertEqual(publish["env"], {
            "AWS_ACCESS_KEY_ID": "${{ secrets.NIX_CACHE_ACCESS_KEY_ID }}",
            "AWS_SECRET_ACCESS_KEY": "${{ secrets.NIX_CACHE_SECRET_ACCESS_KEY }}",
            "NIX_CACHE_SIGNING_KEY": "${{ secrets.NIX_CACHE_SIGNING_KEY }}",
        })

    def test_prepare_lists_realized_outputs_without_building(self):
        step = self.step("main", "Prepare checked outputs")
        listing = self.root / "eidetica-publish-paths"
        checked = [
            "path-info", ".#legacyPackages.x86_64-linux.test.artifacts.archive",
            ".#checks.x86_64-linux.build", ".#checks.x86_64-linux.test",
        ]
        targets = ["path-info", ".#legacyPackages.x86_64-linux.test.artifacts.out"]
        for targets_exit in (0, 1):
            with self.subTest(targets_exit=targets_exit):
                result = self.run_script(step["run"], TARGETS_EXIT=str(targets_exit))
                self.assertEqual(result.returncode, 0, result.stderr)
                calls = self.calls()
                self.assertEqual(calls[0][:3], ["eval", "--raw", ".#checks.x86_64-linux"])
                self.assertEqual(calls[1:], [checked, targets])
                expected = [CHECKED, ARCHIVE] + ([] if targets_exit else [TARGETS])
                self.assertEqual(listing.read_text().split(), expected)
                self.assertEqual("::notice::" in result.stdout, bool(targets_exit))
        # An unrealized check output is fatal: nothing is built to fill the gap.
        result = self.run_script(step["run"], LOCAL_EXIT="42")
        self.assertEqual(result.returncode, 42, result.stderr)
        self.assertEqual(self.calls()[1:], [checked])
        self.assertFalse(any(c[0] == "build" for c in self.calls()))

    def test_main_steps_stop_before_publication_on_failure(self):
        steps = [s for s in self.jobs["main"]["steps"] if "run" in s]
        failures = {
            "Checks + integration": "BUILD_EXIT",
            "Prepare checked outputs": "LOCAL_EXIT",
            "Publish checked outputs": "COPY_EXIT",
        }
        for failed in (None, *failures):
            with self.subTest(failed=failed):
                executed, copied = [], False
                (self.root / "eidetica-publish-paths").unlink(missing_ok=True)
                for step in steps:
                    changes = {"CI_EVENT_NAME": "push"}
                    if step["name"] == "Publish checked outputs":
                        changes["NIX_CACHE_SIGNING_KEY"] = "FAKE-TEST-KEY"
                    if step["name"] == failed:
                        changes[failures[failed]] = "42"
                    result = self.run_script(step["run"], **changes)
                    executed.append(step["name"])
                    copied = copied or any(c[0] == "copy" for c in self.calls())
                    if result.returncode:
                        break
                self.assertEqual(result.returncode, 42 if failed else 0, result.stderr)
                self.assertEqual(executed[-1], failed or "Publish checked outputs")
                self.assertEqual(copied, failed in (None, "Publish checked outputs"))

    def test_no_publication_bypass(self):
        self.assertNotIn("workflow_call", self.ci["on"])
        self.assertNotIn("workflow_run", self.ci["on"])
        self.assertNotIn("download-artifact", self.ci_text)
        self.assertNotIn("upload-artifact", self.ci_text)
        publish = yaml.load(
            (ROOT / ".github/workflows/publish.yml").read_text(), Loader=yaml.BaseLoader
        )
        # The dev path only runs as CI's dependent job; manual dispatch requires
        # a release tag and never takes the dev path.
        self.assertEqual(
            publish["on"]["workflow_dispatch"]["inputs"]["tag_name"]["required"], "true"
        )
        self.assertNotIn("dev", publish["on"]["workflow_dispatch"]["inputs"])


class Publication(Stubbed):
    def setUp(self):
        super().setUp()
        self.list = self.root / "eidetica-publish-paths"
        self.list.write_text(f"{CHECKED}\n{ARCHIVE}\n{CHECKED}\n")

    def run_helper(self, **changes):
        return self.run_script(
            "bash .github/scripts/publish-cache.sh",
            NIX_CACHE_SIGNING_KEY="FAKE-TEST-KEY", **changes,
        )

    def test_signs_only_listed_realized_outputs_and_cleans_key(self):
        result = self.run_helper()
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.calls()
        paths = sorted({CHECKED, ARCHIVE})
        self.assertEqual(calls[0], ["path-info", *paths])
        self.assertEqual(calls[1][:4], ["store", "sign", "--recursive", "--key-file"])
        self.assertEqual(calls[1][5:], paths)
        self.assertEqual(calls[2][:2], ["copy", "--to"])
        self.assertIn("compression=zstd", calls[2][2])
        self.assertEqual(calls[2][3:], paths)
        self.assertEqual(len(calls), 3)
        self.assertFalse(list(self.root.glob("eidetica-cache-key.*")))
        self.assertNotIn("FAKE-TEST-KEY", result.stdout + result.stderr)

    def test_publication_failures_stay_fatal_and_key_is_removed(self):
        for failure in ("LOCAL_EXIT", "SIGN_EXIT", "COPY_EXIT"):
            with self.subTest(failure=failure):
                result = self.run_helper(**{failure: "42"})
                self.assertEqual(result.returncode, 42, result.stderr)
                self.assertFalse(list(self.root.glob("eidetica-cache-key.*")))
                if failure != "COPY_EXIT":
                    self.assertFalse(any(c[0] == "copy" for c in self.calls()))

    def test_requires_exact_main_repository_event_and_ref(self):
        for changes in (
            {"GITHUB_EVENT_NAME": "pull_request"},
            {"GITHUB_EVENT_NAME": "pull_request_target"},
            {"GITHUB_EVENT_NAME": "workflow_call"},
            {"GITHUB_EVENT_NAME": "release"},
            {"GITHUB_REF": "refs/heads/topic"},
            {"GITHUB_REF": "refs/tags/v1.2.3"},
            {"GITHUB_REPOSITORY": "arcuru/chaz"},
            {"GITHUB_REPOSITORY_OWNER": "other"},
        ):
            with self.subTest(changes=changes):
                result = self.run_helper(**changes)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("refused", result.stderr)
                self.assertEqual(self.calls(), [])
        self.assertEqual(self.run_helper(GITHUB_EVENT_NAME="workflow_dispatch").returncode, 0)

    def test_empty_missing_partial_or_foreign_lists_fail_before_signing(self):
        for content in (None, "", "\n", "/tmp/foreign\n", f"{CHECKED}\ninvalid\n",
                        f"{CHECKED}\n/nix/store/short-name\n"):
            with self.subTest(content=content):
                self.list.unlink(missing_ok=True)
                if content is not None:
                    self.list.write_text(content)
                result = self.run_helper()
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.calls(), [])

    def test_missing_signing_key_fails_before_signing(self):
        result = self.run_script(
            "bash .github/scripts/publish-cache.sh", NIX_CACHE_SIGNING_KEY=""
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(any(c[0] in ("store", "copy") for c in self.calls()))
        self.assertFalse(list(self.root.glob("eidetica-cache-key.*")))


if __name__ == "__main__":
    unittest.main()
