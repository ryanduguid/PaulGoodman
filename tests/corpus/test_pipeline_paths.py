"""Failure and edge paths of the smaller builder stages and their shared file helpers."""

from __future__ import annotations

import contextlib
import tempfile
import unittest
import uuid
from pathlib import Path
from unittest import mock

from fadden import corpus_paths

RID = "C2099A00001"


class TemporaryDirectoryTestCase(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)


class BuilderLayoutTests(TemporaryDirectoryTestCase):
    def test_a_fadden_directory_outside_a_checkout_is_refused(self) -> None:
        script = self.base / "fadden" / "stage.py"
        script.parent.mkdir()
        with self.assertRaisesRegex(RuntimeError, "corpus stages require a source checkout"):
            corpus_paths.require_builder_layout(script)

    def test_an_unrecognised_script_directory_is_refused(self) -> None:
        script = self.base / "elsewhere" / "stage.py"
        script.parent.mkdir()
        with self.assertRaisesRegex(RuntimeError, "corpus stages require a source checkout"):
            corpus_paths.require_builder_layout(script)

    def test_a_root_without_markdown_has_no_unlisted_titles(self) -> None:
        self.assertIsNone(corpus_paths.refuse_unlisted_titles(self.base, set()))

    def test_only_unlisted_nondot_entries_are_refused_and_left_in_place(self) -> None:
        markdown = self.base / "markdown"
        markdown.mkdir()
        (markdown / RID).mkdir()
        (markdown / ".cache").mkdir()
        corpus_paths.refuse_unlisted_titles(self.base, {RID})

        stale = markdown / "C2099A00002"
        stale.write_text("stale title", encoding="utf-8")
        with self.assertRaisesRegex(RuntimeError, stale.name):
            corpus_paths.refuse_unlisted_titles(self.base, {RID})
        self.assertEqual(stale.read_text(encoding="utf-8"), "stale title")

    def test_metadata_is_one_literal_markdown_line(self) -> None:
        self.assertEqual(
            corpus_paths.markdown_text("Croft Manor [draft]\r\n*Title*"),
            r"Croft Manor \[draft\] \*Title\*",
        )


class AtomicTextWriterTests(TemporaryDirectoryTestCase):
    def test_a_colliding_staging_name_is_skipped(self) -> None:
        destination = self.base / "out.txt"
        taken, free = uuid.UUID(int=1), uuid.UUID(int=2)
        (self.base / f".out.txt.{taken.hex}.tmp").write_text("someone else's", encoding="utf-8")
        with mock.patch.object(corpus_paths.uuid, "uuid4", side_effect=[taken, free]):
            with corpus_paths.atomic_text_writer(destination) as target:
                target.write("mine")
        self.assertEqual(destination.read_text(encoding="utf-8"), "mine")
        self.assertEqual(
            (self.base / f".out.txt.{taken.hex}.tmp").read_text(encoding="utf-8"), "someone else's")

    def _fail_to_open(self, cleanup_error: OSError | None) -> OSError:
        original = OSError(24, "simulated descriptor exhaustion")
        unlink = mock.patch.object(Path, "unlink", side_effect=cleanup_error) if cleanup_error \
            else contextlib.nullcontext()
        with mock.patch.object(corpus_paths.os, "fdopen", side_effect=original), unlink:
            with self.assertRaises(OSError) as caught:
                with corpus_paths.atomic_text_writer(self.base / "out.txt"):
                    self.fail("the writer must not yield without a stream")
        self.assertIs(caught.exception, original)
        self.assertFalse((self.base / "out.txt").exists())
        return caught.exception

    def test_a_stream_that_cannot_open_removes_its_staging_file(self) -> None:
        self._fail_to_open(None)
        self.assertEqual(list(self.base.iterdir()), [])

    def test_a_staging_file_already_gone_keeps_the_original_error(self) -> None:
        error = self._fail_to_open(FileNotFoundError(2, "already gone"))
        self.assertIsNone(error.__cause__)

    def test_a_staging_file_that_cannot_be_removed_is_chained(self) -> None:
        cleanup = PermissionError(13, "locked")
        error = self._fail_to_open(cleanup)
        self.assertIs(error.__cause__, cleanup)


if __name__ == "__main__":
    unittest.main()
