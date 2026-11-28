"""Tests for the check names ci.yml reports when no code changed.

Run from the repository root:
    python3 -m unittest discover -s .github/scripts -t .github/scripts
"""

from __future__ import annotations

import pathlib
import re
import unittest

REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]

CODE_CHANGED = "    if: needs.changes.outputs.code == 'true'\n"
NO_CODE_CHANGED = "    if: needs.changes.outputs.code == 'false'\n"


def jobs(workflow: str) -> dict[str, str]:
    body = workflow.split("\njobs:\n", 1)[1]
    parts = re.split(r"^  ([a-z][a-z-]*):\n", body, flags=re.MULTILINE)
    return dict(zip(parts[1::2], parts[2::2]))


def leg_names(job: str) -> list[str]:
    template = re.search(r"^    name: (.+)$", job, flags=re.MULTILINE).group(1)
    labels = re.findall(r"^            label: (.+)$", job, flags=re.MULTILINE)
    return [template.replace("${{ matrix.label }}", label) for label in labels]


class NoCodeJobTests(unittest.TestCase):
    def setUp(self) -> None:
        self.jobs = jobs((REPO_ROOT / ".github/workflows/ci.yml").read_text())

    def test_no_code_reports_every_leg_of_the_skipped_matrices(self) -> None:
        self.assertIn("no-code", self.jobs, "ci.yml has no no-code job")
        reported = re.findall(r"^          - (.+)$", self.jobs["no-code"], flags=re.MULTILINE)
        legs = leg_names(self.jobs["check"]) + leg_names(self.jobs["engine-probe"])

        self.assertEqual(len(legs), 6, legs)
        self.assertEqual(sorted(reported), sorted(legs))
        self.assertIn("    name: ${{ matrix.check }}\n", self.jobs["no-code"])

    def test_no_code_runs_exactly_when_the_matrices_skip(self) -> None:
        for name in ("check", "engine-probe"):
            with self.subTest(job=name):
                self.assertIn("    needs: changes\n", self.jobs[name])
                self.assertIn(CODE_CHANGED, self.jobs[name])
        self.assertIn("    needs: changes\n", self.jobs["no-code"])
        self.assertIn(NO_CODE_CHANGED, self.jobs["no-code"])

    def test_the_changes_job_only_ever_writes_true_or_false(self) -> None:
        writes = re.findall(r"^\s+code=(\S+)$", self.jobs["changes"], flags=re.MULTILINE)

        self.assertEqual(sorted(set(writes)), ["false", "true"])

    def test_no_code_holds_no_token(self) -> None:
        self.assertIn("    permissions: {}\n", self.jobs["no-code"])
        self.assertNotIn("uses:", self.jobs["no-code"])


if __name__ == "__main__":
    unittest.main()
