"""Regression tests for failure handling and redistributable outputs.

These tests use only the standard library and temporary directories.  They do
not call the Register API or require a built corpus.
"""
import contextlib
import importlib.util
import io
import json
import re
import shutil
import sys
import tempfile
import unittest
import warnings
from pathlib import Path
from unittest import mock

REPO = Path(__file__).resolve().parents[2]
STAGE = REPO / "fadden"


def stage_file(name):
    candidate = STAGE / name
    return candidate if candidate.exists() else REPO / name


def load_module(name, path):
    module_dir = str(Path(path).parent)
    sys.path.insert(0, module_dir)
    try:
        spec = importlib.util.spec_from_file_location(name, path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module
    finally:
        sys.path.remove(module_dir)



class ProseAndSourceTests(unittest.TestCase):
    def test_no_document_count_is_not_hard_coded_in_maintained_prose(self):
        for name in ("README.md", "BUILD.md", "src/fadden/retry13.rs", "src/fadden/extract.rs"):
            with self.subTest(name=name):
                text = (stage_file(name)).read_text(encoding="utf-8")
                self.assertNotRegex(text, r"(?i)\bthirteen titles\b")


class GeneratedReadmeTests(unittest.TestCase):
    def test_the_corpus_readme_states_no_staleness_counts_it_never_observed(self):
        """check_current is a separate command run after this README is
        written, so any run's counts baked into the template go stale on the
        next rebuild while the interpolated figures beside them stay correct."""
        source = (REPO / "src" / "fadden" / "finalize.rs").read_text(encoding="utf-8")
        stale = re.search(r"\d[\d,]* unchanged", source)
        self.assertIsNone(stale, stale.group(0) if stale else "")
        self.assertIn("{notcurrent} titles are not the current text", source)

    def test_the_corpus_readme_does_not_file_those_titles_as_superseded(self):
        """They are their own bucket.  check_current separates them, and
        sources.json gives their reason as the in-force version having no
        published compilation on the Register.  Calling them superseded tells
        the reader to re-download a document that does not exist, at a URL that
        answers 404 - the miscategorisation the pipeline was fixed to stop
        making."""
        source = (REPO / "src" / "fadden" / "finalize.rs").read_text(encoding="utf-8")
        paragraphs = [" ".join(p.split()) for p in re.split(r"\n\s*\n", source)
                      if "`titles_not_current_version`" in p]
        self.assertTrue(paragraphs, "no README paragraph cites the bucket")
        for paragraph in paragraphs:
            self.assertNotIn("superseded compilation", paragraph, paragraph)
        staleness = source.split("## Checking staleness", 1)[1].split("## Licence", 1)[0]
        self.assertIn("no published compilation", " ".join(staleness.split()))

    def test_scope_states_the_measured_corpus_size(self):
        """scope.md called the corpus about a gigabyte; the EPUBs, markdown and
        rates measure about 150 MB (27 September 2026). Hold the figure so the
        old claim cannot return beside the reasons the corpus is not shipped."""
        scope = " ".join((REPO / "docs" / "scope.md").read_text(encoding="utf-8").split())
        self.assertIn("It is about 150 MB", scope)
        self.assertNotIn("gigabyte", scope)

    def test_this_repositorys_readme_does_not_file_those_titles_as_superseded(self):
        """The same claim, in the README a reader meets first.  The generated
        text was corrected and this one was not, and the test above reads only
        finalize's template, so nothing held the sibling.  BUILD.md and check_current
        keep these titles out of the superseded bucket precisely because that
        bucket says re-download, at a URL that answers 404."""
        readme = (REPO / "docs" / "scope.md").read_text(encoding="utf-8")
        paragraphs = [" ".join(p.split()) for p in re.split(r"\n\s*\n", readme)
                      if "version_is_current" in p]
        self.assertTrue(paragraphs, "no scope paragraph cites the flag")
        for paragraph in paragraphs:
            self.assertNotIn("superseded compilation", paragraph, paragraph)
            self.assertIn("no published compilation", paragraph, paragraph)


class StateRegisterLicenceTests(unittest.TestCase):
    """A state or territory register needs its terms read and recorded in
    scope.md before any stage reads it.  Hold one row per register with one
    of the 5 readings, and hold NOTICE to the attribution wording of every
    register a stage reads."""

    READINGS = {"allowed with attribution", "allowed for personal or internal use only",
                "unclear", "not allowed", "unread"}
    REGISTERS = ("NSW", "Vic", "Qld", "WA", "SA", "Tas", "ACT", "NT")

    def test_scope_records_a_reading_for_each_register(self):
        scope = (REPO / "docs" / "scope.md").read_text(encoding="utf-8")
        section = scope.split("### State and territory registers", 1)
        self.assertEqual(len(section), 2, "no state register section in scope.md")
        rows = {}
        for line in section[1].splitlines():
            cells = [c.strip() for c in line.strip().strip("|").split("|")]
            if line.startswith("| ") and cells[0] in self.REGISTERS:
                rows[cells[0]] = cells
        self.assertEqual(set(rows), set(self.REGISTERS))
        for name, cells in rows.items():
            with self.subTest(register=name):
                self.assertIn(cells[-1].strip("`"), self.READINGS)
                self.assertRegex(cells[1], r"<https://[^>]+>")

    def test_notice_carries_the_queensland_attribution(self):
        notice = " ".join((REPO / "NOTICE").read_text(encoding="utf-8").split())
        qld = notice[notice.index("Queensland Legislation website under"):]
        for required in (
                "Based on content from the Queensland Legislation website at [full date "
                "of download]. For the latest information on Queensland Government "
                "legislation please go to https://www.legislation.qld.gov.au.",
                "https://creativecommons.org/licenses/by/4.0/",
                "(c) The State of Queensland (Office of the Queensland Parliamentary Counsel)",
                "material under other specified licences are excluded",
                "with the date filled in"):
            with self.subTest(required=required):
                self.assertIn(required, qld)

    def test_notice_carries_the_tasmanian_attribution(self):
        notice = " ".join((REPO / "NOTICE").read_text(encoding="utf-8").split())
        tas = notice[notice.index("Tasmanian Legislation website under"):]
        for required in (
                "Based on material from the Tasmanian Legislation website at [full date "
                "of download]. For the latest information on Tasmanian Government "
                "legislation please go to www.legislation.tas.gov.au.",
                "https://creativecommons.org/licenses/by/4.0/",
                "©The State of Tasmania (The Department of Premier and Cabinet) 2026",
                "material under other specified licences are excluded",
                "with the date filled in"):
            with self.subTest(required=required):
                self.assertIn(required, tas)


class ShippedIntermediateTests(unittest.TestCase):
    def test_build_md_records_that_the_shipped_manifest_predates_the_fix(self):
        """manifest_md.json still holds the pre-fix parse of F2025L00281 - the
        published corpus was built before the volume gate was per volume - and
        BUILD.md cites those same figures as the evidence of the bug.  Quote the
        manifest's own numbers here, so regenerating it fails this test rather
        than leaving BUILD.md describing a state that no longer exists."""
        manifest = json.loads((STAGE / "manifest_md.json").read_text(encoding="utf-8"))
        entry = [a for a in manifest if a["id"] == "F2025L00281"]
        self.assertEqual(len(entry), 1)
        build = " ".join((REPO / "BUILD.md").read_text(encoding="utf-8").split())
        self.assertIn("predate the volume-gate fix", build)
        self.assertIn("sections=%d, words=%d" % (entry[0]["sections"], entry[0]["words"]),
                      build)
        # The table stack landed after the same run, so it moves the same
        # figures and has to be recorded in the same breath as the volume gate.
        self.assertIn("table stack", build)
        # The dated evidence document owns the historical table and caveat.
        readme = " ".join((REPO / "docs" / "build-evidence.md").read_text(encoding="utf-8").split())
        self.assertIn("predates the volume-gate fix", readme)
        self.assertIn("table-stack fix", readme)

    def test_the_published_dist_figures_are_the_shipped_manifests_arithmetic(self):
        """README and RELEASE_NOTES sized the redistributable subset at 21,728
        rows and 6,068,848 words for this snapshot.  dist.py counts the kept
        titles' sections.jsonl lines, so the subset is the manifest less the
        titles pii_flagged.json names - 21,596 rows - and its word total cannot
        exceed the whole corpus's, which the same README gives as 6,041,512.
        Derive the figures here, so regenerating the manifest fails this test
        rather than leaving both files sizing a distribution nobody can build.
        """
        manifest = json.loads((STAGE / "manifest_md.json").read_text(encoding="utf-8"))
        flagged = json.loads((STAGE / "pii_flagged.json").read_text(encoding="utf-8"))
        dropped = {entry["register_id"] for entry in flagged}
        kept = [a for a in manifest if a["id"] not in dropped]
        # "934 titles and 21,596 rows", however each file qualifies "titles".
        subset = r"%s(?: \w+)? titles and %s rows" % (
            re.escape(f"{len(kept):,}"),
            re.escape(f"{sum(a['sections'] for a in kept):,}"))
        corpus_words = sum(a["words"] for a in manifest)
        for name in ("docs/scope.md", "RELEASE_NOTES.md"):
            with self.subTest(document=name):
                text = " ".join((REPO / name).read_text(encoding="utf-8").split())
                self.assertRegex(text, subset)
                for figure in re.findall(r"([\d,]+) (?:body )?words", text):
                    self.assertLessEqual(int(figure.replace(",", "")), corpus_words)

    def test_the_removed_row_total_is_not_described_as_the_flagged_row_count(self):
        """README and RELEASE_NOTES both called the 188 rows dist.py removes
        the rows "naming private individuals".  pii_flagged.json records only
        169 that way - 188 is the total row count of the 12 flagged titles, and
        the other 19 rows are signature blocks and figure placeholders that no
        scan matched.  They still go, because dist.py drops a flagged title by
        register id (`if rid in removed: continue`) and never reads row_ids, so
        the removal is whole-title.  Both figures and the relationship between
        them are asserted here, so restating either as the other fails.
        """
        flagged = json.loads((STAGE / "pii_flagged.json").read_text(encoding="utf-8"))
        rows_total = sum(entry["rows_total"] for entry in flagged)
        rows_flagged = sum(entry["rows_flagged"] for entry in flagged)
        # The flagged count is exactly the enumerated rows, and is the smaller
        # of the two.  A rescan that collapses the gap needs the documents
        # reworded, not this test weakened.
        self.assertEqual(rows_flagged,
                         sum(len(entry["row_ids"]) for entry in flagged))
        self.assertLess(rows_flagged, rows_total)
        # The prose claim is only true because of what the dist stage does.
        dist = (REPO / "src" / "fadden" / "dist.rs").read_text(encoding="utf-8")
        self.assertIn("if removed.contains(&rid)", dist)
        self.assertNotIn("row_ids", dist)
        # "12 titles hold 188 rows between them, of which ... 169 ... naming
        # private individuals", however each document phrases it.
        accounting = re.compile(
            r"\b%s titles\b.{0,80}?\b%s rows\b.{0,120}?\b%s\b.{0,40}?"
            r"nam\w* private individuals" % (
                len(flagged), f"{rows_total:,}", f"{rows_flagged:,}"))
        for name in ("docs/scope.md", "RELEASE_NOTES.md"):
            with self.subTest(document=name):
                text = " ".join((REPO / name).read_text(encoding="utf-8").split())
                self.assertRegex(text, accounting)
                # ... and the consequence of the whole-title drop, which is why
                # the larger figure is the one subtracted from the manifest.
                self.assertRegex(text, r"drops a flagged title whole")
                self.assertRegex(
                    text, r"all %s\b.{0,60}?leave the distribution" % f"{rows_total:,}")


class DocumentedCommandTests(unittest.TestCase):
    """The commands and module paths the repository's own documents hand a
    reader.  Each one here was wrong on an unmodified checkout."""

    def test_the_documented_verification_command_scopes_discovery_to_the_corpus(self):
        """CONTRIBUTING.md promises a standard-library-only run and then
        discovered from `tests`, which errors on the 2 radar modules that
        import pytest, so the file's only verification instruction was red on a
        clean clone.  The command must scope discovery to the corpus half."""
        contributing = (REPO / "CONTRIBUTING.md").read_text(encoding="utf-8")
        command = "python -m unittest discover -s tests/corpus -t . -v"
        self.assertIn(command, contributing)
        self.assertNotIn("unittest discover -s tests -v", contributing)

    def test_every_test_module_the_documents_name_exists(self):
        """CONTRIBUTING.md and .gitignore named pre-merge paths: the modules
        moved under tests/corpus/ when the radar half merged in.  The
        .gitignore reference is the justification for keeping manifest_md.json
        committed, so a reader who follows it cannot check the reason."""
        for name in ("CONTRIBUTING.md", ".gitignore"):
            text = (REPO / name).read_text(encoding="utf-8")
            for reference in re.findall(r"\btests[./][\w./]*test_\w+", text):
                with self.subTest(document=name, reference=reference):
                    parts = reference.replace(".py", "").replace("/", ".").split(".")
                    # A dotted reference may carry a class and method after the
                    # module, so accept the longest prefix that is a real file.
                    self.assertTrue(
                        any(REPO.joinpath(*parts[:stop - 1],
                                          parts[stop - 1] + ".py").exists()
                            for stop in range(len(parts), 1, -1)),
                        "no module answers this path")


class PostMergeNamingTests(unittest.TestCase):
    """Names left behind by the radar half's merge into this repository."""

    def test_llms_txt_names_the_repository_pyproject_declares(self):
        """llms.txt is the file an agent reads for orientation and it gave the
        archived tax-radar-au repository as this project's home, which RADAR.md
        records as archived and pyproject.toml contradicts."""
        pyproject = (REPO / "pyproject.toml").read_text(encoding="utf-8")
        homepage = re.search(r'^Homepage = "([^"]+)"$', pyproject, flags=re.M)
        self.assertIsNotNone(homepage)
        llms = (REPO / "llms.txt").read_text(encoding="utf-8")
        self.assertIn("- **Repository**: %s" % homepage.group(1), llms)

    def test_llms_txt_names_the_release_version_declares(self):
        """llms.txt told agents the current release was 0.1.6 after VERSION had
        moved on, so it has to follow the version file."""
        version = (REPO / "VERSION").read_text(encoding="utf-8").strip()
        llms = (REPO / "llms.txt").read_text(encoding="utf-8")
        self.assertIn("- **Version**: %s, released as the tag `v%s`." % (version, version), llms)

    def test_no_document_sends_a_reader_to_the_archived_repository(self):
        """README.md linked the consumer half to the repository it merged out
        of rather than to RADAR.md beside it."""
        for name in ("README.md", "BUILD.md", "CONTRIBUTING.md", "RADAR.md",
                     "RELEASE_NOTES.md", "RELEASING.md", "llms.txt"):
            with self.subTest(document=name):
                text = (REPO / name).read_text(encoding="utf-8")
                self.assertNotIn("github.com/ryanduguid/tax-radar-au", text)

    def test_no_document_claims_a_rename_from_a_name_to_itself(self):
        """BUILD.md and CONTRIBUTING.md both read "`tax-radar-au` (formerly
        `tax-radar-au`)".  RADAR.md carries the real former artefact name."""
        for name in ("README.md", "BUILD.md", "CONTRIBUTING.md", "RADAR.md",
                     "RELEASE_NOTES.md", "RELEASING.md", "llms.txt"):
            text = (REPO / name).read_text(encoding="utf-8")
            for current, former in re.findall(r"`([^`\n]+)` \(formerly `([^`\n]+)`\)", text):
                with self.subTest(document=name, name=current):
                    self.assertNotEqual(current, former)


class SourceWarningTests(unittest.TestCase):
    def test_no_tracked_module_compiles_with_a_warning(self):
        """An invalid escape in a non-raw literal is a DeprecationWarning now
        and a SyntaxError later, and ci.yml records why that matters here: a
        test file that stops being collected leaves every leg green, so the
        module would disappear from the run without a red anywhere."""
        for folder in ("fadden", "tax_radar_au", "tests", "tools"):
            for path in sorted((REPO / folder).rglob("*.py")):
                with self.subTest(module=path.relative_to(REPO).as_posix()):
                    with warnings.catch_warnings(record=True) as caught:
                        warnings.simplefilter("always")
                        compile(path.read_text(encoding="utf-8"), str(path), "exec")
                    self.assertEqual([str(w.message) for w in caught], [])


class PathBoundaryTests(unittest.TestCase):
    def test_distribution_boundary_rejects_a_root_directory_junction(self):
        paths = load_module("corpus_paths_root_junction", STAGE / "corpus_paths.py")
        with (
            mock.patch.object(paths.os.path, "islink", return_value=False),
            mock.patch.object(paths.os.path, "isjunction", return_value=True, create=True),
            mock.patch.object(paths.os, "walk") as walk,
        ):
            with self.assertRaisesRegex(ValueError, "junction"):
                paths.reject_symlinks("corpus")
        walk.assert_not_called()

    def test_distribution_boundary_rejects_windows_directory_junctions(self):
        paths = load_module("corpus_paths_junction", STAGE / "corpus_paths.py")
        with (
            mock.patch.object(paths.os, "walk", return_value=[("corpus", ["junction"], [])]) as walk,
            mock.patch.object(paths.os.path, "islink", return_value=False),
            mock.patch.object(paths, "is_reparse_point", return_value=False),
            mock.patch.object(
                paths.os.path,
                "isjunction",
                side_effect=lambda candidate: candidate == paths.os.path.join("corpus", "junction"),
                create=True,
            ),
        ):
            with self.assertRaisesRegex(ValueError, "junction"):
                paths.reject_symlinks("corpus")
        walk.assert_called_once_with("corpus", followlinks=False)

    def test_distribution_boundary_rejects_other_windows_reparse_points(self):
        paths = load_module("corpus_paths_reparse", STAGE / "corpus_paths.py")
        with (
            mock.patch.object(paths.os.path, "islink", return_value=False),
            mock.patch.object(paths.os.path, "isjunction", return_value=False, create=True),
            mock.patch.object(paths, "is_reparse_point", return_value=True),
            mock.patch.object(paths.os, "walk") as walk,
        ):
            with self.assertRaisesRegex(ValueError, "reparse point"):
                paths.reject_symlinks("corpus")
        walk.assert_not_called()

    def test_distribution_boundary_accepts_an_ordinary_tree(self):
        paths = load_module("corpus_paths_ordinary", STAGE / "corpus_paths.py")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "corpus"
            (root / "nested").mkdir(parents=True)
            (root / "nested" / "file.txt").write_bytes(b"x")
            self.assertIsNone(paths.reject_symlinks(root))

    def test_register_id_and_contained_child_reject_traversal(self):
        paths = load_module("corpus_paths_regression", STAGE / "corpus_paths.py")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "corpus"
            root.mkdir()
            self.assertEqual(paths.register_id("F2020L01498"), "F2020L01498")
            self.assertEqual(paths.register_id("C2004A05138"), "C2004A05138")
            with self.assertRaises(ValueError):
                paths.register_id("../../outside")
            with self.assertRaises(ValueError):
                paths.child(root, "..", "outside")
            with self.assertRaises(ValueError):
                paths.child(root, ".")

    def test_root_layout_supports_checkout_and_deployed_build(self):
        paths = load_module("corpus_paths_layout", STAGE / "corpus_paths.py")
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            checkout_script = base / "checkout" / "extract.py"
            checkout_script.parent.mkdir()
            checkout_script.write_text("# placeholder\n", encoding="utf-8")
            self.assertEqual(Path(paths.corpus_root(checkout_script)), checkout_script.parent / "corpus")

            packaged = base / "checkout-pkg" / "fadden" / "extract.py"
            packaged.parent.mkdir(parents=True)
            packaged.write_text("# placeholder\n", encoding="utf-8")
            self.assertEqual(Path(paths.corpus_root(packaged)), packaged.parent.parent / "corpus")

            deployed_script = base / "corpus-root" / "build" / "extract.py"
            deployed_script.parent.mkdir(parents=True)
            deployed_script.write_text("# placeholder\n", encoding="utf-8")
            self.assertEqual(Path(paths.corpus_root(deployed_script)), deployed_script.parent.parent)

            deployed_pkg = base / "corpus-root-pkg" / "build" / "fadden" / "extract.py"
            deployed_pkg.parent.mkdir(parents=True)
            deployed_pkg.write_text("# placeholder\n", encoding="utf-8")
            self.assertEqual(Path(paths.corpus_root(deployed_pkg)), deployed_pkg.parent.parent.parent)

    def test_check_current_uses_checkout_output_or_deployed_root(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            checkout = base / "checkout"
            checkout.mkdir()
            for name in ("check_current.py", "corpus_paths.py", "http_fetch.py"):
                shutil.copy2(stage_file(name), checkout / name)
            checkout_module = load_module("check_current_checkout", checkout / "check_current.py")
            self.assertEqual(Path(checkout_module.ROOT), checkout / "corpus")

            deployed = base / "deployed"
            deployed.mkdir()
            (deployed / "sources.json").write_text("{}", encoding="utf-8")
            for name in ("check_current.py", "corpus_paths.py", "http_fetch.py"):
                shutil.copy2(stage_file(name), deployed / name)
            deployed_module = load_module("check_current_deployed", deployed / "check_current.py")
            self.assertEqual(Path(deployed_module.ROOT), deployed)


class StalenessBucketTests(unittest.TestCase):
    def _run(self, base, response):
        module = load_module("check_current_buckets", base / "check_current.py")
        module.fetch_json = response if callable(response) else lambda _url: response
        buffer = io.StringIO()
        with mock.patch.object(module.sys, "argv", ["check_current.py"]), \
                mock.patch.object(module.time, "sleep"):
            with contextlib.redirect_stdout(buffer):
                module.main()
        return buffer.getvalue()

    def _corpus(self, base):
        (base / "sources.json").write_text(json.dumps({
            "retrieved": "2026-08-03",
            "titles": [{"register_id": "F2020L01498", "name": "Demo Instrument",
                        "collection": "LegislativeInstrument",
                        "compilation_number": "4", "compilation_date": "2025-06-30"}],
        }), encoding="utf-8")
        for name in ("check_current.py", "corpus_paths.py", "http_fetch.py"):
            shutil.copy2(stage_file(name), base / name)

    def test_a_current_version_with_no_document_is_its_own_bucket(self):
        """Filing these under "re-download these" sends the reader at a URL
        that answers 404: the compilation does not exist yet."""
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            self._corpus(base)
            out = self._run(base, {"value": [{
                "titleId": "F2020L01498", "start": "2026-07-01T00:00:00Z",
                "compilationNumber": "5", "registerId": None}]})
            self.assertIn("no compilation published: 1", out)
            self.assertIn("NO COMPILATION PUBLISHED", out)
            self.assertNotIn("SUPERSEDED, re-download these", out)
            self.assertIn("superseded: 0", out)

    def test_a_published_later_compilation_is_reported_as_superseded(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            self._corpus(base)
            out = self._run(base, {"value": [{
                "titleId": "F2020L01498", "start": "2026-07-01T00:00:00Z",
                "compilationNumber": "5", "registerId": "F2026C00099"}]})
            self.assertIn("superseded: 1", out)
            self.assertIn("SUPERSEDED, re-download these", out)
            self.assertIn("no compilation published: 0", out)

    def test_a_failed_current_lookup_is_not_inferred_from_history(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            self._corpus(base)
            module = load_module("check_current_transport_failure", base / "check_current.py")
            responses = [None, {"value": [{"titleId": "F2020L01498"}]}]
            calls = []

            def response(_url):
                calls.append(_url)
                return responses.pop(0)

            module.fetch_json = response
            buffer = io.StringIO()
            with mock.patch.object(module.sys, "argv", ["check_current.py"]), \
                    mock.patch.object(module.time, "sleep"):
                with contextlib.redirect_stdout(buffer):
                    module.main()

            out = buffer.getvalue()
            self.assertEqual(len(calls), 1)
            self.assertIn("no longer in force: 0", out)
            self.assertIn("lookup failed: 1", out)

    def test_a_wrong_title_response_is_a_lookup_failure(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            self._corpus(base)
            out = self._run(base, {"value": [{
                "titleId": "F9999L99999", "start": "2026-07-01T00:00:00Z",
                "compilationNumber": "5", "registerId": "F2026C00099"}]})
            self.assertIn("no longer in force: 0", out)
            self.assertIn("lookup failed: 1", out)

    def test_a_title_with_history_but_no_current_version_is_no_longer_in_force(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            self._corpus(base)
            responses = iter([
                {"value": []},
                {"value": [{"titleId": "F2020L01498"}]},
            ])
            out = self._run(base, lambda _url: next(responses))
            self.assertIn("no longer in force: 1", out)
            self.assertIn("NO LONGER IN FORCE", out)
            self.assertIn("lookup failed: 0", out)

    def test_an_unchanged_current_version_stays_out_of_action_buckets(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            self._corpus(base)
            out = self._run(base, {"value": [{
                "titleId": "F2020L01498", "start": "2025-06-30T00:00:00Z",
                "compilationNumber": "4", "registerId": "F2025C00042",
            }]})
            self.assertIn("unchanged: 1", out)
            self.assertIn("superseded: 0", out)
            self.assertIn("no compilation published: 0", out)
            self.assertIn("no longer in force: 0", out)
            self.assertIn("lookup failed: 0", out)


class CheckCurrentStatusTests(unittest.TestCase):
    """The staleness checker must read the Register's status before compilations."""

    TITLE = {"register_id": "F2016L01256", "name": "Fabricated repealed instrument",
             "collection": "LegislativeInstrument", "compilation_number": "1",
             "compilation_date": "2016-08-02"}

    def run_check(self, responses):
        current = load_module("check_current_status_regression", STAGE / "check_current.py")
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "sources.json").write_text(json.dumps(
                {"retrieved": "2016-08-02", "titles": [self.TITLE]}), encoding="utf-8")
            stdout = io.StringIO()
            with mock.patch.object(current, "ROOT", str(root)), \
                    mock.patch.object(current, "fetch_json", side_effect=responses), \
                    mock.patch.object(current.time, "sleep"), \
                    mock.patch.object(sys, "argv", ["check_current.py"]), \
                    contextlib.redirect_stdout(stdout):
                current.main()
        return stdout.getvalue()

    def test_a_repealed_current_row_is_reported_as_no_longer_in_force(self):
        # The shape of the public API record for F2016L01256: isCurrent true,
        # status Repealed and no document id. Read compilation-first it looked
        # like a compilation the Register had yet to publish.
        output = self.run_check([{"value": [{
            "titleId": "F2016L01256", "start": "2026-08-12T00:00:00Z",
            "compilationNumber": None, "registerId": None,
            "isCurrent": True, "status": "Repealed"}]}])
        self.assertIn("no longer in force: 1", output)
        self.assertIn("no compilation published: 0", output)
        self.assertIn("NO LONGER IN FORCE", output)
        self.assertNotIn("NO COMPILATION PUBLISHED", output)

    def test_an_in_force_row_without_a_document_is_still_an_unpublished_compilation(self):
        output = self.run_check([{"value": [{
            "titleId": "F2016L01256", "start": "2026-08-12T00:00:00Z",
            "compilationNumber": None, "registerId": None,
            "isCurrent": True, "status": "InForce"}]}])
        self.assertIn("no compilation published: 1", output)
        self.assertIn("no longer in force: 0", output)
        self.assertIn("NO COMPILATION PUBLISHED", output)

    def test_the_query_asks_the_register_for_the_status_field(self):
        current = load_module("check_current_select_regression", STAGE / "check_current.py")
        seen = []

        def record(url):
            seen.append(url)
            return {"value": [{"titleId": "F2016L01256", "start": "2016-08-02T00:00:00Z",
                               "compilationNumber": "1", "registerId": "F2016C00001",
                               "isCurrent": True, "status": "InForce"}]}

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "sources.json").write_text(json.dumps(
                {"retrieved": "2016-08-02", "titles": [self.TITLE]}), encoding="utf-8")
            with mock.patch.object(current, "ROOT", str(root)), \
                    mock.patch.object(current, "fetch_json", side_effect=record), \
                    mock.patch.object(current.time, "sleep"), \
                    mock.patch.object(sys, "argv", ["check_current.py"]), \
                    contextlib.redirect_stdout(io.StringIO()):
                current.main()
        self.assertTrue(seen)
        self.assertIn("compilationNumber,registerId,status", seen[0])


if __name__ == "__main__":
    unittest.main()
