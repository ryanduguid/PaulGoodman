# Copyright 2026 Ryan Duguid
# SPDX-License-Identifier: LicenseRef-Duguid-Public-Attribution-1.0

import contextlib
import io
import json
import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from fadden import quick
from fadden.__main__ import main as dispatch

HIT = "1. Income Tax Assessment Act 1997: 8-1 General deductions\n"


class Stages:
    """Stands in for the Rust binary and records what ran."""

    def __init__(self, hit: str = HIT, statuses: tuple[str, ...] = ("ok",),
                 indexed: tuple[str, ...] = quick.PRESET) -> None:
        self.ran: list[str] = []
        self.hit = hit
        self.statuses = statuses
        self.indexed = indexed

    def stage(self, _binary: str, scratch: Path, *args: str, capture: bool = False) -> str:
        self.ran.append(" ".join(args[:2]) if args[0] == "search" else args[0])
        if args[0] == "finalize":
            (scratch.parent / "sources.json").write_text("{}", encoding="utf-8")
        if args[:2] == ("search", "build"):
            self.write_index(args[4])
        if capture:
            return self.hit
        return ""

    def write_index(self, path: str) -> None:
        connection = sqlite3.connect(path)
        connection.execute("CREATE TABLE rows (register_id TEXT)")
        connection.executemany("INSERT INTO rows VALUES (?)", [(rid,) for rid in self.indexed])
        connection.commit()
        connection.close()

    def download(self, scratch: str, epub_dir: str) -> None:
        self.ran.append("download")
        rows = [{"status": status} for status in self.statuses]
        Path(scratch, "manifest_raw.json").write_text(json.dumps(rows), encoding="utf-8")
        self.epub_dir = epub_dir

    def retry(self, _scratch: str, _epub_dir: str) -> None:
        self.ran.append("retry13")

    @contextlib.contextmanager
    def patched(self):  # type: ignore[no-untyped-def]
        with mock.patch.object(quick, "stage", self.stage), \
                mock.patch.object(quick.download, "main", self.download), \
                mock.patch.object(quick.retry13, "main", self.retry):
            yield self


class QuickTests(unittest.TestCase):
    def setUp(self) -> None:
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.base = Path(folder.name)
        self.out = self.base / "law"

    def staging(self) -> Path:
        return self.base / "law.building"

    def test_preparing_lays_out_the_preset_and_what_the_stages_read(self) -> None:
        self.assertEqual(quick.prepare(self.out), self.staging())
        self.assertFalse(self.out.exists())
        scratch = self.staging() / "build"
        titles = json.loads((scratch / "titles_all.json").read_text(encoding="utf-8"))
        self.assertEqual({title["id"] for title in titles}, set(quick.PRESET))
        self.assertTrue(all(title["isPrincipal"] for title in titles))
        for name in quick.SUPPORT:
            self.assertTrue((scratch / name).is_file(), name)

    def test_an_empty_folder_is_taken_and_a_full_one_refused(self) -> None:
        self.out.mkdir()
        with Stages().patched():
            quick.build("fadden-bin", self.out)
        self.assertTrue((self.out / "sources.json").is_file())
        self.assertFalse(self.staging().exists())
        with self.assertRaisesRegex(quick.QuickError, "not empty"):
            quick.prepare(self.out)
        self.assertFalse(self.staging().exists())

    def test_an_earlier_partial_build_is_left_for_the_operator(self) -> None:
        self.staging().mkdir()
        (self.staging() / "download_log.txt").write_text("x", encoding="utf-8")
        with self.assertRaisesRegex(quick.QuickError, "earlier partial build"):
            quick.prepare(self.out)
        self.assertTrue((self.staging() / "download_log.txt").is_file())

    def test_a_missing_parent_is_an_error_not_a_new_tree(self) -> None:
        with self.assertRaises(OSError):
            quick.prepare(self.base / "absent" / "law")

    def test_a_preset_act_missing_from_the_title_list_stops_the_run(self) -> None:
        with mock.patch.object(quick, "PRESET", ("C2099A00001",)), \
                self.assertRaisesRegex(quick.QuickError, "every preset Act"):
            quick.prepare(self.out)

    def test_the_stages_run_in_pipeline_order_into_the_folder(self) -> None:
        with Stages().patched() as stages:
            hit = quick.build("fadden-bin", self.out)
        self.assertEqual(hit, HIT)
        self.assertTrue((self.out / "index.sqlite").is_file())
        self.assertEqual(stages.ran, [
            "versions", "download", "extract", "pii_scan", "pii_scan2", "finalize",
            "search build", "search query",
        ])
        self.assertEqual(stages.epub_dir, str(self.staging() / "epub"))

    def test_a_title_with_no_published_document_takes_the_recovery_route(self) -> None:
        with Stages(statuses=("ok", "no_epub")).patched() as stages:
            quick.build("fadden-bin", self.out)
        at = stages.ran.index("download")
        self.assertEqual(stages.ran[at + 1:at + 3], ["probe13", "retry13"])

    def test_a_build_with_no_hit_is_a_failure(self) -> None:
        with Stages(hit="0 rows.\n").patched(), \
                self.assertRaisesRegex(quick.QuickError, "holds no row"):
            quick.build("fadden-bin", self.out)

    def test_a_late_failure_leaves_a_consumer_nothing_to_read(self) -> None:
        self.out.mkdir()
        printed = io.StringIO()
        with Stages(hit="0 rows.\n").patched(), \
                mock.patch.object(quick, "rust_binary", return_value="bin"), \
                contextlib.redirect_stdout(printed), contextlib.redirect_stderr(printed):
            self.assertEqual(quick.main(["--out", str(self.out)]), 1)
        self.assertEqual(list(self.out.iterdir()), [])
        self.assertTrue((self.staging() / "sources.json").is_file())
        self.assertIn(str(self.staging()), printed.getvalue())
        self.assertNotIn("AUS_ACCOUNTING_CORPUS_ROOT", printed.getvalue())

    def test_a_title_the_stages_dropped_stops_the_run(self) -> None:
        dropped = quick.PRESET[-1]
        printed = io.StringIO()
        with Stages(indexed=quick.PRESET[:-1]).patched(), \
                mock.patch.object(quick, "rust_binary", return_value="bin"), \
                contextlib.redirect_stdout(printed), contextlib.redirect_stderr(printed):
            self.assertEqual(quick.main(["--out", str(self.out)]), 1)
        self.assertIn(f"the index holds no rows for {dropped}", printed.getvalue())
        self.assertNotIn("Built and indexed", printed.getvalue())
        self.assertFalse(self.out.exists())

    def test_a_folder_filled_during_the_build_is_kept(self) -> None:
        self.out.mkdir()
        stages = Stages()
        run_stage = stages.stage

        def filling(*args: str, capture: bool = False) -> str:
            if capture:
                (self.out / "held.txt").write_text("x", encoding="utf-8")
            return run_stage(*args, capture=capture)

        with stages.patched(), mock.patch.object(quick, "stage", filling), \
                self.assertRaisesRegex(quick.QuickError, "partial build stays"):
            quick.build("fadden-bin", self.out)
        self.assertEqual([path.name for path in self.out.iterdir()], ["held.txt"])

    def test_a_stage_runs_with_its_files_in_the_folder(self) -> None:
        script = "import os; print(os.environ['FADDEN_MODULE_DIR'])"
        got = quick.stage(sys.executable, self.base, "-c", script, capture=True)
        self.assertEqual(got.strip(), str(self.base))
        self.assertEqual(quick.stage(sys.executable, self.base, "-c", "pass"), "")

    def test_a_failed_stage_stops_the_run(self) -> None:
        with self.assertRaisesRegex(quick.QuickError, "the -c stage exited 3"):
            quick.stage(sys.executable, self.base, "-c", "raise SystemExit(3)")

    def test_the_command_prints_the_hit_the_time_and_the_folder_setting(self) -> None:
        printed = io.StringIO()
        with Stages().patched(), mock.patch.object(quick, "rust_binary", return_value="bin"), \
                contextlib.redirect_stdout(printed):
            code = quick.main(["--out", str(self.out)])
        self.assertEqual(code, 0)
        text = printed.getvalue()
        self.assertIn(HIT, text)
        self.assertRegex(text, r"Built and indexed 9 Acts in \d+ seconds\.")
        self.assertIn("search_tax_legislation", text)
        self.assertIn(f"AUS_ACCOUNTING_CORPUS_ROOT={self.out.resolve()}\n", text)

    def test_the_command_reports_a_failure_and_exits_1(self) -> None:
        self.out.mkdir()
        (self.out / "held.txt").write_text("x", encoding="utf-8")
        printed = io.StringIO()
        with mock.patch.object(quick, "rust_binary", return_value="bin"), \
                contextlib.redirect_stderr(printed):
            self.assertEqual(quick.main(["--out", str(self.out)]), 1)
        self.assertIn("not empty", printed.getvalue())
        self.assertTrue((self.out / "held.txt").exists())
        self.assertFalse(self.staging().exists())

    def test_the_command_needs_the_rust_binary(self) -> None:
        printed = io.StringIO()
        with mock.patch.object(quick, "rust_binary", return_value=None), \
                contextlib.redirect_stderr(printed):
            self.assertEqual(quick.main(["--out", str(self.out)]), 1)
        self.assertIn("cargo build", printed.getvalue())
        self.assertFalse(self.out.exists())

    def test_the_dispatcher_forwards_the_arguments(self) -> None:
        with mock.patch.object(quick, "main", return_value=0) as stage:
            self.assertEqual(dispatch(["quick", "--out", "law"]), 0)
        stage.assert_called_once_with(["--out", "law"])

    def test_download_and_retry13_write_only_where_they_are_told(self) -> None:
        scratch = self.base / "build"
        scratch.mkdir()
        epub = scratch.parent / "epub"
        (scratch / "acts_resolved.json").write_text("[]", encoding="utf-8")
        printed = io.StringIO()
        with contextlib.redirect_stdout(printed):
            quick.download.main(str(scratch), str(epub))
            (scratch / "probe13.json").write_text("[]", encoding="utf-8")
            quick.retry13.main(str(scratch), str(epub))
        self.assertTrue(epub.is_dir())
        self.assertTrue((scratch / "manifest_raw.json").is_file())
        self.assertTrue((scratch / "download_log.txt").is_file())
        self.assertTrue((scratch / "retry13_patch.json").is_file())
