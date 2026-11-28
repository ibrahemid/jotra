"""Tests for the perf gate in scripts/gate.sh.

Run from the repository root:
    python3 -m unittest discover -s scripts -p 'test_*.py'
"""

from __future__ import annotations

import os
import pathlib
import re
import stat
import subprocess
import tempfile
import unittest

REPO_ROOT = pathlib.Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "gate.sh"
CI = REPO_ROOT / ".github" / "workflows" / "ci.yml"

FAKE = """#!/bin/sh
printf '%s JOTRA_PERF_GATE=%s %s\\n' "$(basename "$0")" "${JOTRA_PERF_GATE:-}" "$*" >> "$CALLS"
[ "$(basename "$0")" != "$FAIL_TOOL" ]
"""


class PerfGateTests(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory(prefix="jotra-gate-test-")
        self.addCleanup(tmp.cleanup)
        self.root = pathlib.Path(tmp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        for tool in ("pnpm", "node", "npx", "cargo"):
            fake = self.bin / tool
            fake.write_text(FAKE)
            fake.chmod(fake.stat().st_mode | stat.S_IXUSR)
        self.calls = self.root / "calls.txt"
        self.logs = self.root / "logs"

    def gate(self, *gates: str, fail_tool: str = "") -> subprocess.CompletedProcess[str]:
        env = {
            **os.environ,
            "PATH": f"{self.bin}{os.pathsep}{os.environ['PATH']}",
            "CLAUDE_PROJECT_DIR": str(REPO_ROOT),
            "JOTRA_GATE_LOG_DIR": str(self.logs),
            "CALLS": str(self.calls),
            "FAIL_TOOL": fail_tool,
        }
        env.pop("JOTRA_PERF_GATE", None)
        return subprocess.run(
            ["bash", str(SCRIPT), *gates], capture_output=True, text=True, env=env, check=False
        )

    def recorded(self) -> list[str]:
        return self.calls.read_text().splitlines() if self.calls.exists() else []

    def test_perf_runs_the_steps_of_the_ci_editor_perf_gate(self) -> None:
        result = self.gate("perf")

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertRegex(result.stdout, r"^PASS perf\s+\d+s$")
        self.assertEqual(
            self.recorded(),
            [
                "pnpm JOTRA_PERF_GATE=1 exec vitest run src/__tests__/lib/document-counts-perf.test.ts"
                " -t counting a 50 MiB document --disable-console-intercept",
                "pnpm JOTRA_PERF_GATE= exec playwright-core install --no-shell chromium",
                "node JOTRA_PERF_GATE=1 scripts/perf-editor.mjs",
            ],
        )

    def test_the_gate_and_ci_run_the_same_budget_commands(self) -> None:
        step = CI.read_text().split("      - name: Editor perf gate\n", 1)[1].split("\n\n", 1)[0]
        ci_commands = re.findall(r"^          (pnpm exec vitest .+|node .+)$", step, flags=re.MULTILINE)

        self.gate("perf")
        gate_commands = [
            line.split(" ", 2)[2] for line in self.recorded() if "JOTRA_PERF_GATE=1" in line
        ]

        self.assertEqual(len(ci_commands), 2, step)
        self.assertEqual(
            [command.replace('"', "") for command in ci_commands],
            [f"{tool} {args}" for tool, args in zip(("pnpm", "node"), gate_commands)],
        )

    def test_a_failed_budget_fails_the_gate_and_stops_it(self) -> None:
        result = self.gate("perf", fail_tool="pnpm")

        self.assertEqual(result.returncode, 1)
        self.assertRegex(result.stdout, r"^FAIL perf\s+\d+s  log: ")
        self.assertEqual(len(self.recorded()), 1)

    def test_perf_is_not_in_the_default_set(self) -> None:
        result = self.gate()

        self.assertNotIn("perf", result.stdout)
        self.assertFalse(any("perf-editor" in line for line in self.recorded()))


if __name__ == "__main__":
    unittest.main()
