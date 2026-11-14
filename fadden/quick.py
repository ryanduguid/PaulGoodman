# Copyright 2026 Ryan Duguid
# SPDX-License-Identifier: LicenseRef-Duguid-Public-Attribution-1.0

"""Build and index a preset of principal Acts in an empty folder.

Each step is the pipeline's own stage, so the fetches are the same polite ones:
`versions` for the current compilations and `download` for the EPUBs, at the
Register's Crawl-delay. The folder takes the shape of a deployed build, with
`build/` holding the stage files and the corpus at its root, and it is the folder
the Australian Accounting MCP server reads, so the stages run in a sibling
`<folder>.building` and the folder appears only once every preset Act is indexed.
Nothing is fetched from anywhere but the Register and nothing is published.

`--in-force-on YYYY-07-01` also holds, under `compilations/<date>/`, each preset
Act's compilation in force on that 1 July, where the build holds no other copy of it.
"""

from __future__ import annotations

import argparse
import contextlib
import datetime
import json
import shutil
import sqlite3
import sys
import time
from pathlib import Path
from typing import Any, Sequence

from . import download, retry13
from .__main__ import run_binary, rust_binary

# The 8 principal Acts the MCP server ranks first, then the SIS Act.
PRESET = (
    "C2004A05138",  # Income Tax Assessment Act 1997
    "C2004A00446",  # A New Tax System (Goods and Services Tax) Act 1999
    "C1936A00027",  # Income Tax Assessment Act 1936
    "C1953A00001",  # Taxation Administration Act 1953
    "C2004A03280",  # Fringe Benefits Tax Assessment Act 1986
    "C2004A04402",  # Superannuation Guarantee (Administration) Act 1992
    "C2004A05128",  # Income Tax (Transitional Provisions) Act 1997
    "C2004A03348",  # Income Tax Rates Act 1986
    "C2004A04633",  # Superannuation Industry (Supervision) Act 1993
)
MODULE = Path(__file__).resolve().parent
# What pii_scan2 and finalize read beside the stage files.
SUPPORT = ("pii_contact_allowlist.json", "check_current.py", "corpus_paths.py", "http_fetch.py")
SMOKE_QUERY = ("general", "deductions")
FAILURES = (OSError, ValueError, RuntimeError, sqlite3.Error)


class QuickError(RuntimeError):
    """A step failed; the named folder was not touched."""


def lay_out(scratch: Path, titles: list[dict[str, object]]) -> None:
    """A new module directory holding the titles and what the stages read beside them."""
    scratch.mkdir(parents=True)
    for name in SUPPORT:
        shutil.copyfile(MODULE / name, scratch / name)
    (scratch / "titles_all.json").write_text(json.dumps(titles, indent=1), encoding="utf-8")


def prepare(out: Path) -> Path:
    """Refuse a folder holding anything, then lay out a staging folder beside it."""
    if out.exists() and any(out.iterdir()):
        raise QuickError(f"{out} is not empty; name an absent or empty folder")
    rows = json.loads((MODULE / "titles_principal.json").read_text(encoding="utf-8"))
    titles = [row for row in rows if row["id"] in PRESET]
    if len(titles) != len(PRESET):
        raise QuickError("titles_principal.json no longer holds every preset Act")
    staging = out.with_name(f"{out.name}.building")
    if staging.exists():
        raise QuickError(f"{staging} holds an earlier partial build; remove it first")
    staging.mkdir()
    lay_out(staging / "build", titles)
    return staging


def stage(binary: str, scratch: Path, *args: str, capture: bool = False) -> str:
    """One Rust stage with its files in `scratch`; a failed stage stops the run."""
    done = run_binary(binary, args, module_dir=scratch, capture=capture)
    if done.returncode:
        raise QuickError(f"the {args[0]} stage exited {done.returncode}")
    return done.stdout.decode("utf-8") if capture else ""


def recover_missing(binary: str, scratch: Path, epub_dir: str) -> None:
    """A current version with no published document takes the probe13 and retry13 route."""
    manifest = json.loads((scratch / "manifest_raw.json").read_text(encoding="utf-8"))
    if any(row["status"] == "no_epub" for row in manifest):
        stage(binary, scratch, "probe13")
        retry13.main(str(scratch), epub_dir)


def check_preset(index: Path) -> None:
    """Every preset Act must hold indexed rows, or the build is not the preset."""
    with contextlib.closing(sqlite3.connect(f"{index.as_uri()}?mode=ro", uri=True)) as connection:
        held = {row[0] for row in connection.execute("SELECT DISTINCT register_id FROM rows")}
    missing = [register_id for register_id in PRESET if register_id not in held]
    if missing:
        raise QuickError(f"the index holds no rows for {', '.join(missing)}")


def assemble(binary: str, staging: Path) -> str:
    """Every step from the laid-out staging folder to a checked index; returns a hit."""
    scratch = staging / "build"
    epub_dir = str(staging / "epub")
    stage(binary, scratch, "versions")
    download.main(str(scratch), epub_dir)
    recover_missing(binary, scratch, epub_dir)
    for name in ("extract", "pii_scan", "pii_scan2"):
        stage(binary, scratch, name)
    # The README and sources.json must describe a named preset, not a keyword search.
    stage(binary, scratch, "finalize", "--preset")
    index = staging / "index.sqlite"
    stage(binary, scratch, "search", "build", str(staging), "--out", str(index))
    check_preset(index)
    hit = stage(binary, scratch, "search", "query", str(index), *SMOKE_QUERY, "--limit", "1", capture=True)
    if not hit.startswith("1. "):
        raise QuickError(f"the build holds no row for {' '.join(SMOKE_QUERY)!r}")
    return hit


def extracted(scratch: Path) -> list[dict[str, Any]]:
    """The manifest row of each compilation a module directory extracted."""
    rows: list[dict[str, Any]] = json.loads(
        (scratch / "manifest_md.json").read_text(encoding="utf-8"))
    return rows


def check_windows(rows: list[dict[str, Any]]) -> None:
    """Refuse a build in which 2 compilations of one title cover the same date.

    `overlaps` in src/fadden/compilations.rs applies the same rule to a distribution.
    """
    windows: dict[str, list[tuple[str, str | None]]] = {}
    for row in rows:
        windows.setdefault(row["id"], []).append(
            (row["versionStart"], row.get("current_version_start")))
    for rid, held in windows.items():
        held.sort()
        for (start, end), (later, _) in zip(held, held[1:]):
            if end is None or end > later:
                raise QuickError(f"2 compilations of {rid} overlap: the one from {later} "
                                 f"starts inside the window of the one from {start}")


def add_compilations(binary: str, staging: Path, dates: Sequence[str]) -> None:
    """Each date's earlier compilations under compilations/<date>/, in the root's layout.

    `versions --in-force-on` leaves out, with its reason, a title whose version on the
    date is the current one or cannot be fetched; a compilation the root or an earlier
    date already holds is left out here, and the windows of one title must not overlap.
    A date left with nothing to fetch gets no folder.
    """
    scratch = staging / "build"
    titles = json.loads((scratch / "titles_all.json").read_text(encoding="utf-8"))
    rows = extracted(scratch)
    folder = staging / "compilations"
    for on in sorted(set(dates)):
        build = folder / on
        scratch = build / "build"
        lay_out(scratch, titles)
        stage(binary, scratch, "versions", "--in-force-on", on)
        target = scratch / "acts_resolved.json"
        resolved = json.loads(target.read_text(encoding="utf-8"))
        kept = {(row["id"], row["versionStart"]) for row in rows}
        wanted = [row for row in resolved if (row["id"], row["versionStart"]) not in kept]
        print(f"compilations/{on}: {len(wanted)} to fetch, "
              f"{len(resolved) - len(wanted)} already held, "
              f"{len(titles) - len(resolved)} with none to fetch, listed above")
        if not wanted:
            shutil.rmtree(build)
            continue
        download.write_json_atomic(str(target), wanted, indent=1)
        download.main(str(scratch), str(build / "epub"))
        for name in ("extract", "pii_scan", "pii_scan2"):
            stage(binary, scratch, name)
        got = extracted(scratch)
        missing = sorted({row["id"] for row in wanted} - {row["id"] for row in got})
        if missing:
            raise QuickError(f"compilations/{on} holds no rows for {', '.join(missing)}")
        rows += got
    check_windows(rows)
    if folder.is_dir() and not any(folder.iterdir()):
        folder.rmdir()


def in_force_date(value: str) -> str:
    """A 1 July as YYYY-07-01, which names its compilations/ folder."""
    try:
        valid = datetime.date.fromisoformat(value).isoformat() == value
    except ValueError:
        valid = False
    if not valid or not value.endswith("-07-01"):
        raise argparse.ArgumentTypeError(f"{value!r} is not a 1 July as YYYY-07-01")
    return value


def publish(staging: Path, out: Path) -> None:
    """Move the finished build to `out`; `rmdir` refuses a folder filled since `prepare`."""
    if out.exists():
        out.rmdir()
    staging.rename(out)


def build(binary: str, out: Path, dates: Sequence[str] = ()) -> str:
    """Build beside `out`, then move the checked result to it; returns a query hit."""
    staging = prepare(out)
    try:
        hit = assemble(binary, staging)
        if dates:
            add_compilations(binary, staging, dates)
        publish(staging, out)
    except FAILURES as error:
        raise QuickError(f"{error}; the partial build stays in {staging}") from error
    return hit


def main(argv: Sequence[str]) -> int:
    parser = argparse.ArgumentParser(
        prog="python -m fadden quick",
        description="Build and index a preset of principal Acts, then print the MCP folder setting.",
    )
    parser.add_argument("--out", required=True, help="an absent or empty folder")
    parser.add_argument(
        "--in-force-on", action="append", default=[], type=in_force_date, metavar="YYYY-07-01",
        help="also hold each Act's earlier compilation in force on this 1 July; repeat for more",
    )
    args = parser.parse_args(argv)
    binary = rust_binary()
    if binary is None:
        print("error: quick needs the Rust fadden binary; build it with "
              "cargo build --locked --release", file=sys.stderr)
        return 1
    out = Path(args.out).resolve()
    started = time.monotonic()
    try:
        hit = build(binary, out, args.in_force_on)
    except FAILURES as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(hit, end="")
    print(f"Built and indexed {len(PRESET)} Acts in {time.monotonic() - started:.0f} seconds.")
    print("Set this for the aus-accounting-mcp server, then call search_tax_legislation:")
    print(f"AUS_ACCOUNTING_CORPUS_ROOT={out}")
    return 0
