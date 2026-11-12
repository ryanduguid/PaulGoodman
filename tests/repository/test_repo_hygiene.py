# Copyright 2026 Ryan Duguid
# SPDX-License-Identifier: LicenseRef-Duguid-Public-Attribution-1.0

"""Guards for repository facts that are otherwise only true by habit.

Each assertion here fails when the exact line it names is removed, so a
packaging, documentation or CI regression cannot land with a green run.
"""

from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

BLOCKED_CHANGE_KINDS = (
    "INCOMPLETE_SCOPE",
    "MISSING_OBSERVATION",
    "LOOKUP_FAILED",
    "CURRENT_NO_PUBLISHED_COMPILATION",
    "BASELINE_NOT_CURRENT",
)


def test_build_output_directories_are_ignored() -> None:
    # The README tells the reader to run `python -m build`, which fills dist/.
    # Unignored, a following `git add -A` commits a wheel and a tarball into a
    # public repository, where they cannot be removed without a history rewrite.
    entries = {line.strip() for line in (ROOT / ".gitignore").read_text(encoding="utf-8").splitlines()}

    assert {"build/", "dist/"} <= entries


def test_readme_names_every_blocked_change_kind() -> None:
    # compare() blocks for 5 distinct reasons. A reader who meets
    # BASELINE_NOT_CURRENT or MISSING_OBSERVATION on a stale baseline needs the
    # README to say what the label and the non-zero exit mean.
    readme = (ROOT / "RADAR.md").read_text(encoding="utf-8")

    for change_kind in BLOCKED_CHANGE_KINDS:
        assert change_kind in readme, f"README does not document {change_kind}"


def test_readme_describes_both_rejection_shapes_not_one() -> None:
    # argparse rejects a malformed command line itself, exit 2 with a usage
    # block and no "blocked:" line, so the README must not promise that prefix
    # for every rejected input. tests/test_cli.py pins the behaviour; this pins
    # the sentence that describes it.
    readme = (ROOT / "RADAR.md").read_text(encoding="utf-8")

    assert "Every rejected input also exits 2" not in readme
    assert "rejected by argparse" in readme


def test_readme_states_that_an_unwritable_stdout_keeps_the_run_exit_status() -> None:
    # The exit-code table above says 0 or 2 and nothing else. A stdout that
    # cannot be written is the one way that promise used to break - the
    # interpreter's exit-time flush replaced the status with 120 - so the
    # README has to say what happens now instead. tests/test_cli.py pins the
    # behaviour; this pins the sentence.
    readme = (ROOT / "RADAR.md").read_text(encoding="utf-8")

    assert "the exit status is still the one the run decided" in readme


def test_readme_states_the_pinned_timestamp_grammar() -> None:
    # The grammar is narrower than datetime.fromisoformat's on every supported
    # interpreter, so a user with a stored artefact needs it written down.
    readme = (ROOT / "RADAR.md").read_text(encoding="utf-8")

    assert "YYYY-MM-DDThh:mm:ss[.ffffff][Z|+hh:mm|-hh:mm]" in readme
    assert "or a single space allowed in place of `T`" in readme


def test_ci_keeps_the_pytest_summary_line_visible() -> None:
    # pyproject already sets addopts = "-q". A second -q on the CI command line
    # makes it -qq, which drops the pass/fail count: a test file that stops
    # being collected then leaves every matrix leg green and identical.
    workflow = (ROOT / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")

    assert 'addopts = "-q"' in (ROOT / "pyproject.toml").read_text(encoding="utf-8")
    assert "pytest -q" not in workflow
    assert "pytest -qq" not in workflow


def test_repository_declares_its_source_only_package_lifecycle() -> None:
    readme = " ".join((ROOT / "README.md").read_text(encoding="utf-8").split())
    radar = (ROOT / "RADAR.md").read_text(encoding="utf-8")

    assert "**Package lifecycle:** source-only." in readme
    assert "not published to PyPI" in readme
    assert "plain `pip install`" not in radar


def test_readme_routes_agents_to_the_published_corpus_snapshot() -> None:
    # A corpus- release ships a verified 9-Act snapshot; the README once said
    # releases carry no ready-made corpus, sending agents to build one instead.
    readme = " ".join((ROOT / "README.md").read_text(encoding="utf-8").split())

    assert "not a ready-made corpus" not in readme
    assert "releases/tag/corpus-" in readme
    assert "AUS_ACCOUNTING_CORPUS_ROOT" in readme
    # The server's setup lived only in another repository's README; each host's
    # line, the Windows and macOS checksum commands and the check stay here.
    assert "claude mcp add --scope user aus-accounting -e AUS_ACCOUNTING_CORPUS_ROOT=" in readme
    assert "codex mcp add aus-accounting --env AUS_ACCOUNTING_CORPUS_ROOT=" in readme
    assert '"env": { "AUS_ACCOUNTING_CORPUS_ROOT":' in readme
    assert "aus-accounting-mcp==0.8.0" in readme
    assert "Get-FileHash" in readme
    assert "shasum -a 256 -c SHA256SUMS" in readme
    assert "aus-accounting-mcp doctor" in readme


def test_comparison_names_the_published_corpus_snapshot() -> None:
    # docs/comparison.md said this builder "needs a local build" after a
    # corpus- release made the 9-Act snapshot downloadable.
    comparison = " ".join((ROOT / "docs" / "comparison.md").read_text(encoding="utf-8").split())

    assert "needs a local build," not in comparison
    assert "you build the corpus yourself" not in comparison
    assert "releases/tag/corpus-" in comparison
