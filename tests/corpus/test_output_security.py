"""Contained paths, atomic writes and publication recovery regressions."""

from __future__ import annotations

import contextlib
import io
import json
import subprocess  # nosec B404 - isolated Python import regression
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from fadden import corpus_paths

from tests.corpus import test_regressions as regressions

REPOSITORY = Path(__file__).resolve().parents[2]
STAGE = REPOSITORY / "fadden"


class PathAndWriteTests(unittest.TestCase):
    def test_full_fraction_grammar_is_portable_and_flat_fadden_roots_work(self):
        for fraction in ("", *("." + "1" * length for length in range(1, 8))):
            for offset in ("", "Z", "+10:00", "-00:59"):
                value = "2099-01-01T12:34:56" + fraction + offset
                with self.subTest(timestamp=value):
                    self.assertEqual(corpus_paths.version_date(value), "2099-01-01")
        with tempfile.TemporaryDirectory() as temporary:
            flat = Path(temporary) / "fadden"
            flat.mkdir()
            (flat / "sources.json").write_text("{}", encoding="utf-8")
            corpus_paths.require_builder_layout(flat / "check_current.py")

    def test_network_stages_reject_filter_injection_before_requesting(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            module = regressions.load_module("injection_check_current", STAGE / "check_current.py")
            injected = "C2099A00001' or isCurrent eq true"
            module.ROOT = str(root)
            (root / "sources.json").write_text(json.dumps({
                "retrieved": "2099-01-01", "titles": [{"register_id": injected}]
            }), encoding="utf-8")
            with mock.patch.object(module, "fetch_json") as fetch,                     mock.patch.object(sys, "argv", ["check_current"]), contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaises(ValueError):
                    module.main()
            fetch.assert_not_called()

    def test_identifiers_and_complete_dates_are_checked(self):
        for value in ("C2099A00001' or true", "C２０９９A00001", "../C2099A00001", None):
            with self.subTest(value=value), self.assertRaises(ValueError):
                corpus_paths.register_id(value)
        for value in ("2099-02-30", "2099-01-01/../../", "2099-01-01T99:00:00Z",
                      "2099-01-01T00:00:00+00:99", "2099-01-01junk", None):
            with self.subTest(value=value), self.assertRaises(ValueError):
                corpus_paths.version_date(value)
        self.assertEqual(corpus_paths.version_date("2099-01-01T00:00:00.1234567Z"), "2099-01-01")
        for value in ("2099-1-01", "2099-01-01T00:00:00Z", None):
            with self.subTest(day=value), self.assertRaises(ValueError):
                corpus_paths.iso_date(value)
        with tempfile.TemporaryDirectory() as temporary:
            target = Path(temporary) / "out.json"
            corpus_paths.write_json_atomic(target, {"a": [1]}, indent=1)
            self.assertEqual(target.read_text(encoding="utf-8"), '{\n "a": [\n  1\n ]\n}')
        # A missing registerId is an API failure; only an explicit null means no document.
        with self.assertRaises(ValueError):
            corpus_paths.compilation_id({})
        self.assertIsNone(corpus_paths.compilation_id({"registerId": None}))

    def test_atomic_writer_preserves_prior_bytes_and_unowned_temp(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "manifest.json"
            path.write_text("previous\n", encoding="utf-8")
            foreign = Path(str(path) + ".tmp")
            foreign.write_text("foreign staging\n", encoding="utf-8")
            with self.assertRaises(KeyboardInterrupt):
                with corpus_paths.atomic_text_writer(path) as target:
                    target.write("partial replacement")
                    raise KeyboardInterrupt
            self.assertEqual(path.read_text(encoding="utf-8"), "previous\n")
            self.assertEqual(foreign.read_text(encoding="utf-8"), "foreign staging\n")
            self.assertEqual(set(path.parent.iterdir()), {path, foreign})
            with corpus_paths.atomic_text_writer(path) as target:
                target.write("complete replacement\n")
            self.assertEqual(path.read_text(encoding="utf-8"), "complete replacement\n")

    def test_installed_locations_refuse_legacy_stages_before_side_effects(self):
        module = regressions.load_module("installed_guard_check_current", STAGE / "check_current.py")
        with tempfile.TemporaryDirectory() as temporary,                 mock.patch.object(module, "__file__", str(Path(temporary) / "fadden" / "check_current.py")):
            with self.assertRaisesRegex(RuntimeError, "source checkout"):
                module.main()
            self.assertEqual(list(Path(temporary).iterdir()), [])

    def test_clean_package_imports_ignore_bare_helper_modules(self):
        with tempfile.TemporaryDirectory() as temporary:
            hostile = Path(temporary)
            for name in ("corpus_paths", "http_fetch"):
                (hostile / (name + ".py")).write_text('raise AssertionError("bare helper imported")\n', encoding="utf-8")
            script = ("import sys, importlib; sys.path[:0] = " + repr([str(REPOSITORY), str(hostile)]) +
                      "; before = list(sys.path); from fadden import STAGES; import fadden.__main__; "
                      "[importlib.import_module('fadden.' + name) for name in STAGES if name not in fadden.__main__.RUST_STAGES]; "
                      "assert sys.path == before; assert 'corpus_paths' not in sys.modules")
            # Fixed interpreter and test-owned script, with isolated import state.
            # nosemgrep: python.lang.security.audit.dangerous-subprocess-use-audit.dangerous-subprocess-use-audit
            result = subprocess.run([sys.executable, "-I", "-c", script], cwd=temporary,  # nosec B603
                                    text=True, capture_output=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)

if __name__ == "__main__":
    unittest.main()
