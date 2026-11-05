from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import subprocess  # nosec B404 - runs git and the checker with fixed argument lists
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CHECKER = ROOT / ".github" / "ci" / "check_gates.py"
# The canonical checker lives in ryanduguid/release-policy, whose
# tests/test_repository_baseline.sh pins the same digest (SHA-256 after CRLF to LF).
POLICY_DIGEST = "8d48fa32c92d15bb476e28f2ad34400660f62ee337c527c1d9fd9b7f6230f62a"
GATES = (
    (".github/workflows/ci.yml", "tests-gates", ("--exempt", "codecov"), "lint"),
    (".github/workflows/codeql.yml", "codeql-gates", (), "analyze"),
)


def needs(text: str, gate: str) -> list[str]:
    match = re.search(rf"^  {gate}:\n(?:    #.*\n)*    needs: \[(.*)\]$", text, re.MULTILINE)
    assert match is not None, f"{gate} has no single-line needs"
    return [job.strip() for job in match.group(1).split(",")]


def without_job(text: str, job: str) -> str:
    body = re.compile(rf"^  {job}:\n(?:(?:    .*)?\n)+?(?=  \S)", re.MULTILINE)
    removed, count = body.subn("", text)
    assert count == 1, f"cannot remove {job}"
    return re.sub(rf"\b{job}, |, {job}\b", "", removed)


class ReleasePolicyGateTests(unittest.TestCase):
    def pull_request(self, workflow: str, gate: str, exempt: tuple[str, ...], edit) -> subprocess.CompletedProcess[str]:
        """Run the checker on a commit whose parent holds this repository's workflows."""
        with tempfile.TemporaryDirectory(prefix="gates-") as directory:
            shutil.copytree(ROOT / ".github" / "workflows", Path(directory) / ".github" / "workflows")
            env = {key: os.environ[key] for key in ("PATH", "SystemRoot", "TEMP", "TMP") if key in os.environ}
            env.update(HOME=directory, GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)

            def git(*args: str) -> None:
                subprocess.run(  # nosec B603, B607 - fixed git arguments in a temporary repository
                    ["git", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", *args],
                    cwd=directory, env=env, check=True, capture_output=True, timeout=30,
                )

            git("init", "--quiet", "--initial-branch=main")
            git("add", "--all")
            git("commit", "--quiet", "--message", "base")
            path = Path(directory) / workflow
            text = edit(path.read_bytes().decode("utf-8").replace("\r\n", "\n"))
            path.write_bytes(text.encode("utf-8"))
            git("add", "--all")
            git("commit", "--quiet", "--allow-empty", "--message", "change")
            env.update(GITHUB_EVENT_NAME="pull_request", RESULTS=json.dumps({job: {"result": "success"} for job in needs(text, gate)}))
            return subprocess.run(  # nosec B603 - the interpreter and checker are fixed paths
                [sys.executable, "-I", str(CHECKER), workflow, gate, *exempt],
                cwd=directory, env=env, capture_output=True, text=True, timeout=30,
            )

    def test_checker_is_the_release_policy_copy(self) -> None:
        text = CHECKER.read_bytes().replace(b"\r\n", b"\n")
        self.assertEqual(hashlib.sha256(text).hexdigest(), POLICY_DIGEST,
                         "change check_gates.py in ryanduguid/release-policy first, then copy it here unchanged")

    def test_each_gate_accepts_this_repository_workflows(self) -> None:
        for workflow, gate, exempt, _ in GATES:
            with self.subTest(gate=gate):
                result = self.pull_request(workflow, gate, exempt, lambda text: text)
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_each_gate_rejects_a_removed_job(self) -> None:
        for workflow, gate, exempt, job in GATES:
            with self.subTest(gate=gate):
                result = self.pull_request(workflow, gate, exempt, lambda text, job=job: without_job(text, job))
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn("These jobs were removed", result.stderr)


if __name__ == "__main__":
    unittest.main()
