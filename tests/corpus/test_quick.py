# Copyright 2026 Ryan Duguid
# SPDX-License-Identifier: LicenseRef-Duguid-Public-Attribution-1.0

import contextlib
import io
import json
import re
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
                 indexed: tuple[str, ...] = quick.PRESET,
                 versions: dict[str | None, list[tuple[str, str]]] | None = None) -> None:
        self.ran: list[str] = []
        self.calls: list[tuple[str, ...]] = []
        self.hit = hit
        self.statuses = statuses
        self.indexed = indexed
        # The (register id, version start) pairs versions resolves: None for the current
        # versions, else the --in-force-on date.
        self.versions = versions or {}
        self.epub_dirs: list[str] = []

    def stage(self, _binary: str, scratch: Path, *args: str, capture: bool = False) -> str:
        self.ran.append(" ".join(args[:2]) if args[0] == "search" else args[0])
        self.calls.append(args)
        if args[0] == "versions":
            on = args[2] if args[1:2] == ("--in-force-on",) else None
            rows = [{"id": rid, "versionStart": start} for rid, start in self.versions.get(on, [])]
            (scratch / "acts_resolved.json").write_text(json.dumps(rows), encoding="utf-8")
        if args[0] == "extract":
            # The titles download left to extract, as manifest_md.json records them.
            rows = (scratch / "acts_resolved.json").read_text(encoding="utf-8")
            (scratch / "manifest_md.json").write_text(rows, encoding="utf-8")
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
        self.epub_dirs.append(epub_dir)

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

    def test_finalize_is_told_the_titles_are_a_named_preset(self) -> None:
        """A keyword README would describe a search this build never ran."""
        with Stages().patched() as stages:
            quick.build("fadden-bin", self.out)
        self.assertIn(("finalize", "--preset"), stages.calls)

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

    def test_without_a_date_the_build_is_what_it_was(self) -> None:
        """--in-force-on is off by default and changes nothing a default build writes."""
        printed = io.StringIO()
        with Stages().patched() as stages, \
                mock.patch.object(quick, "rust_binary", return_value="bin"), \
                contextlib.redirect_stdout(printed):
            self.assertEqual(quick.main(["--out", str(self.out)]), 0)
        self.assertEqual(stages.calls, [
            ("versions",), ("extract",), ("pii_scan",), ("pii_scan2",), ("finalize", "--preset"),
            ("search", "build", str(self.staging()), "--out", str(self.staging() / "index.sqlite")),
            ("search", "query", str(self.staging() / "index.sqlite"), *quick.SMOKE_QUERY,
             "--limit", "1"),
        ])
        files = sorted(path.relative_to(self.out).as_posix()
                       for path in self.out.rglob("*") if path.is_file())
        self.assertEqual(files, sorted([
            *(f"build/{name}" for name in quick.SUPPORT), "build/titles_all.json",
            "build/acts_resolved.json", "build/manifest_raw.json", "build/manifest_md.json",
            "sources.json", "index.sqlite",
        ]))
        for name in quick.SUPPORT:
            self.assertEqual((self.out / "build" / name).read_bytes(),
                             (quick.MODULE / name).read_bytes(), name)
        rows = json.loads((quick.MODULE / "titles_principal.json").read_text(encoding="utf-8"))
        titles = json.dumps([row for row in rows if row["id"] in quick.PRESET], indent=1)
        self.assertEqual((self.out / "build" / "titles_all.json").read_text(encoding="utf-8"),
                         titles)
        self.assertEqual(
            re.sub(r"in \d+ seconds", "in N seconds", printed.getvalue()),
            f"{HIT}Built and indexed 9 Acts in N seconds.\n"
            "Set this for the aus-accounting-mcp server, then call search_tax_legislation:\n"
            f"AUS_ACCOUNTING_CORPUS_ROOT={self.out.resolve()}\n",
        )

    def test_each_date_holds_the_compilations_no_other_folder_holds(self) -> None:
        itaa97, gst, itaa36 = quick.PRESET[:3]
        versions: dict[str | None, list[tuple[str, str]]] = {
            None: [(itaa97, "2026-10-01"), (gst, "2024-01-01")],
            # The root already holds the GST Act's compilation for this date.
            "2024-07-01": [(itaa97, "2024-07-01"), (gst, "2024-01-01")],
            # 2024-07-01 already holds the ITAA 1997's.
            "2025-07-01": [(itaa97, "2024-07-01"), (itaa36, "2025-03-01")],
            "2026-07-01": [(gst, "2024-01-01")],
        }
        printed = io.StringIO()
        with Stages(versions=versions).patched() as stages, contextlib.redirect_stdout(printed):
            quick.build("fadden-bin", self.out, ["2026-07-01", "2025-07-01", "2024-07-01"])
        folder = self.out / "compilations"
        self.assertEqual(sorted(path.name for path in folder.iterdir()),
                         ["2024-07-01", "2025-07-01"])
        for on, held in (("2024-07-01", [itaa97]), ("2025-07-01", [itaa36])):
            scratch = folder / on / "build"
            rows = json.loads((scratch / "acts_resolved.json").read_text(encoding="utf-8"))
            self.assertEqual([row["id"] for row in rows], held)
            for name in (*quick.SUPPORT, "titles_all.json"):
                self.assertTrue((scratch / name).is_file(), name)
        dated = [call for call in stages.calls if call[1:2] == ("--in-force-on",)]
        self.assertEqual(dated, [("versions", "--in-force-on", on)
                                 for on in ("2024-07-01", "2025-07-01", "2026-07-01")])
        after = stages.ran.index("search query") + 1
        self.assertEqual(stages.ran[after:], [
            *["versions", "download", "extract", "pii_scan", "pii_scan2"] * 2, "versions",
        ])
        self.assertEqual(stages.epub_dirs[1:], [
            str(self.staging() / "compilations" / on / "epub") for on in ("2024-07-01", "2025-07-01")
        ])
        self.assertIn("compilations/2026-07-01: 0 to fetch, 1 already held\n",
                      printed.getvalue())

    def test_a_date_with_nothing_to_fetch_leaves_no_compilations_folder(self) -> None:
        versions: dict[str | None, list[tuple[str, str]]] = {None: [(quick.PRESET[0], "2026-10-01")]}
        with Stages(versions=versions).patched(), contextlib.redirect_stdout(io.StringIO()):
            quick.build("fadden-bin", self.out, ["2025-07-01"])
        self.assertFalse((self.out / "compilations").exists())
        self.assertTrue((self.out / "sources.json").is_file())

    def test_a_date_must_be_a_1_july(self) -> None:
        for bad in ("2025-03-01", "2025-7-01", "2025-02-30"):
            printed = io.StringIO()
            with mock.patch.object(quick, "rust_binary", return_value="bin"), \
                    contextlib.redirect_stderr(printed), self.assertRaises(SystemExit) as stop:
                quick.main(["--out", str(self.out), "--in-force-on", bad])
            self.assertEqual(stop.exception.code, 2)
            self.assertIn("is not a 1 July as YYYY-07-01", printed.getvalue())
        self.assertFalse(self.staging().exists())

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
