"""Check real workflow boundaries and execute the ref/tag shell steps offline."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

import yaml


ROOT = Path(__file__).resolve().parents[2]
MAIN_EVENT = "github.event_name == 'push' || github.event_name == 'workflow_dispatch'"
TRUSTED_MAIN = (
    "github.repository_owner == 'arcuru' && github.ref == 'refs/heads/main' && "
    f"({MAIN_EVENT})"
)
MAIN_QUEUE = (
    f"${{{{ {TRUSTED_MAIN} && 'ci-dev-main' || "
    "format('ci-{0}-{1}', github.event_name, "
    "github.event.pull_request.number || github.run_id) }}"
)


def expression(value):
    return " ".join(value.split())


class PublishWorkflow(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workflows = {
            name: yaml.load(
                (ROOT / f".github/workflows/{name}.yml").read_text(),
                Loader=yaml.BaseLoader,
            )
            for name in ("ci", "publish", "deploy-docs")
        }
        cls.ci, cls.publish, cls.docs = (cls.workflows[n] for n in cls.workflows)

    def run_step(self, job, name, **values):
        step = next(s for s in self.publish["jobs"][job]["steps"] if s["name"] == name)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            # No accidental GitHub/network calls: even a newly introduced lookup
            # reaches this stub and fails the test, including swallowed failures.
            gh = root / "gh"
            gh.write_text('#!/bin/sh\nprintf called > "$GH_CALL_LOG"\nexit 97\n')
            gh.chmod(0o700)
            output = root / "output"
            env = {
                "PATH": f"{root}:{os.defpath}",
                "GITHUB_OUTPUT": str(output),
                "GH_CALL_LOG": str(root / "gh-called"),
                "DEV_PUBLISH": "false",
                "EVENT_NAME": "release",
                "RELEASE_TAG": "v1.2.3",
                "INPUT_TAG": "v1.2.3",
                "IS_PRERELEASE": "true",
                "RUN_SHA": "a" * 40,
                "PLATFORM": "amd64",
                **values,
            }
            subprocess.run([shutil.which("bash"), "-euo", "pipefail", "-c", step["run"]], env=env, check=True)
            self.assertFalse((root / "gh-called").exists(), "unexpected GitHub lookup")
            return dict(line.split("=", 1) for line in output.read_text().splitlines())

    def test_native_queue_spans_ci_and_both_publish_architectures(self):
        self.assertEqual(expression(self.ci["concurrency"]["group"]), MAIN_QUEUE)
        self.assertEqual(self.ci["concurrency"]["cancel-in-progress"], "${{ github.event_name == 'pull_request' }}")
        self.assertNotIn("concurrency", self.publish)
        job = self.ci["jobs"]["publish"]
        self.assertEqual(job["needs"], ["ci", "ci-arm"])
        self.assertEqual(expression(job["if"]), TRUSTED_MAIN)
        self.assertEqual(job["uses"], "./.github/workflows/publish.yml")
        self.assertEqual(job["with"], {"dev": "true"})
        self.assertEqual(self.publish["jobs"]["manifest"]["needs"], "build")
        self.assertEqual(
            [entry["platform"] for entry in self.publish["jobs"]["build"]["strategy"]["matrix"]["include"]],
            ["amd64", "arm64"],
        )

    def test_check_jobs_stay_read_only_and_publisher_secrets_are_explicit(self):
        self.assertEqual(self.ci["permissions"], {"contents": "read"})
        for name in ("ci", "ci-arm"):
            self.assertNotIn("secrets.", yaml.safe_dump(self.ci["jobs"][name]))
        caller = self.ci["jobs"]["publish"]
        self.assertEqual(caller["permissions"], {"contents": "read", "packages": "write"})
        names = {"BENCHER_API_TOKEN", "NIX_CACHE_ACCESS_KEY_ID", "NIX_CACHE_SECRET_ACCESS_KEY", "NIX_CACHE_SIGNING_KEY", "DOCKERHUB_USERNAME", "DOCKERHUB_TOKEN"}
        self.assertEqual(caller["secrets"], {name: "${{ secrets." + name + " }}" for name in names})
        declared = self.publish["on"]["workflow_call"]["secrets"]
        self.assertEqual(declared, {name: {"required": "true"} for name in names})
        self.assertEqual(self.publish["jobs"]["build"]["environment"], "publish")
        self.assertEqual(self.publish["jobs"]["manifest"]["environment"], "release")

    def test_pr_and_full_ci_targets_are_unchanged(self):
        steps = {s["name"]: s for s in self.ci["jobs"]["ci"]["steps"]}
        self.assertEqual(steps["Checks"]["if"], "github.event_name == 'pull_request'")
        self.assertIn(".#legacyPackages.x86_64-linux.ci-pr", steps["Checks"]["run"])
        self.assertEqual(steps["Checks + integration"]["if"], "github.event_name != 'pull_request'")
        self.assertIn(".#legacyPackages.x86_64-linux.ci'", steps["Checks + integration"]["run"])
        self.assertEqual(self.ci["jobs"]["ci-arm"]["if"], "github.event_name != 'pull_request'")

    def test_dev_call_is_main_only_and_release_entries_remain_independent(self):
        self.assertEqual(set(self.publish["on"]), {"workflow_call", "workflow_dispatch", "release"})
        self.assertEqual(self.publish["on"]["workflow_dispatch"]["inputs"]["tag_name"]["required"], "true")
        self.assertEqual(
            expression(self.publish["jobs"]["build"]["if"]),
            "github.repository_owner == 'arcuru' && "
            "((inputs.dev && github.ref == 'refs/heads/main' && "
            f"({MAIN_EVENT})) || (!inputs.dev && "
            "(github.event_name == 'release' || github.event_name == 'workflow_dispatch')))",
        )
        for job in self.publish["jobs"].values():
            self.assertNotIn("actions", job["permissions"])
            self.assertNotIn("always()", job["if"])

    def test_dev_uses_exact_ci_revision_and_dev_tags(self):
        for event in ("push", "workflow_dispatch"):
            with self.subTest(event=event):
                values = {"DEV_PUBLISH": "true", "EVENT_NAME": event, "INPUT_TAG": "", "RELEASE_TAG": ""}
                self.assertEqual(self.run_step("build", "Resolve ref", **values), {"ref": "a" * 40})
                self.assertEqual(self.run_step("build", "Determine tags", **values), {"tags": "dev-amd64"})
                self.assertEqual(self.run_step("manifest", "Determine manifest tags", **values), {"tags": "dev"})

    def test_release_and_manual_retry_keep_versioned_refs_and_tags(self):
        for event in ("release", "workflow_dispatch"):
            with self.subTest(event=event):
                self.assertEqual(self.run_step("build", "Resolve ref", EVENT_NAME=event), {"ref": "v1.2.3"})
                self.assertEqual(self.run_step("build", "Determine tags", EVENT_NAME=event), {"tags": "1.2.3-amd64 1.2-amd64 1-amd64"})
                self.assertEqual(self.run_step("manifest", "Determine manifest tags", EVENT_NAME=event), {"tags": "1.2.3 1.2 1"})

    def test_clean_source_rebuild_and_cache_warming_boundaries(self):
        steps = {s["name"]: s for s in self.publish["jobs"]["build"]["steps"]}
        self.assertEqual(steps["Resolve ref"]["env"]["RUN_SHA"], "${{ github.sha }}")
        self.assertEqual(steps["Checkout"]["with"]["ref"], "${{ steps.ref.outputs.ref }}")
        self.assertEqual(steps["Checkout"]["with"]["persist-credentials"], "false")
        self.assertIn("--option substituters 'https://cache.nixos.org'", steps["Build and push release artifacts"]["run"])
        self.assertEqual(steps["Setup Nix"]["with"]["signing-key"], "${{ secrets.NIX_CACHE_SIGNING_KEY }}")
        for name in ("Warm CI cache", "Install Bencher CLI", "Track artifact sizes"):
            self.assertEqual(steps[name]["if"], "inputs.dev")
        self.assertNotIn("download-artifact", yaml.safe_dump(self.publish))

    def test_dev_and_release_intermediate_images_do_not_mix(self):
        self.assertEqual(self.publish["env"]["BASE_TAG_PREFIX"], "${{ inputs.dev && 'nix-base-dev' || 'nix-base' }}")
        build = next(s for s in self.publish["jobs"]["build"]["steps"] if s["name"] == "Load and push Nix base image")
        cleanup = next(s for s in self.publish["jobs"]["manifest"]["steps"] if s["name"] == "Cleanup intermediate images")
        self.assertIn('${BASE_TAG_PREFIX}-${PLATFORM}', build["run"])
        self.assertIn('${BASE_TAG_PREFIX}-${PLATFORM}', cleanup["run"])

    def test_docs_follow_the_complete_successful_main_pipeline(self):
        trigger = self.docs["on"]["workflow_run"]
        self.assertEqual(trigger["workflows"], ["CI"])
        self.assertEqual(trigger["branches"], ["main"])
        self.assertEqual(
            expression(self.docs["jobs"]["build"]["if"]),
            "github.repository_owner == 'arcuru' && (github.event_name != 'workflow_run' || "
            "(github.event.workflow_run.conclusion == 'success' && github.event.workflow_run.head_branch == 'main' && "
            "(github.event.workflow_run.event == 'push' || github.event.workflow_run.event == 'workflow_dispatch') && "
            "github.event.workflow_run.head_repository.full_name == github.repository))",
        )
        checkout = self.docs["jobs"]["build"]["steps"][0]
        self.assertEqual(checkout["with"]["ref"], "${{ github.event.workflow_run.head_sha || github.ref }}")


if __name__ == "__main__":
    unittest.main()
