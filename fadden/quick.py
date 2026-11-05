# Copyright 2026 Ryan Duguid
# SPDX-License-Identifier: LicenseRef-Duguid-Public-Attribution-1.0

"""Build and index a preset of principal Acts in an empty folder.

Each step is the pipeline's own stage, so the fetches are the same polite ones:
`versions` for the current compilations and `download` for the EPUBs, at the
Register's Crawl-delay. The folder takes the shape of a deployed build, with
`build/` holding the stage files and the corpus at its root, and it is the folder
the Australian Accounting MCP server reads. Nothing is fetched from anywhere but
the Register and nothing is published.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Sequence

from . import download, retry13
from .__main__ import rust_binary

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


class QuickError(RuntimeError):
    """A step failed; the folder stays as that step left it."""


def prepare(out: Path) -> Path:
    """Refuse a folder holding anything, then lay out its stage directory."""
    out.mkdir(exist_ok=True)
    if any(out.iterdir()):
        raise QuickError(f"{out} is not empty; name an absent or empty folder")
    scratch = out / "build"
    scratch.mkdir()
    for name in SUPPORT:
        shutil.copyfile(MODULE / name, scratch / name)
    rows = json.loads((MODULE / "titles_principal.json").read_text(encoding="utf-8"))
    titles = [row for row in rows if row["id"] in PRESET]
    if len(titles) != len(PRESET):
        raise QuickError("titles_principal.json no longer holds every preset Act")
    (scratch / "titles_all.json").write_text(json.dumps(titles, indent=1), encoding="utf-8")
    return scratch


def stage(binary: str, scratch: Path, *args: str, capture: bool = False) -> str:
    """One Rust stage with its files in `scratch`; a failed stage stops the run."""
    env = dict(os.environ, FADDEN_MODULE_DIR=str(scratch))
    # Maintainer CLI; the binary is this checkout's build or the operator's choice.
    done = subprocess.run(  # NOSONAR
        [binary, *args], env=env, check=False, stdout=subprocess.PIPE if capture else None
    )
    if done.returncode:
        raise QuickError(f"the {args[0]} stage exited {done.returncode}")
    return done.stdout.decode("utf-8") if capture else ""


def recover_missing(binary: str, scratch: Path, epub_dir: str) -> None:
    """A current version with no published document takes the probe13 and retry13 route."""
    manifest = json.loads((scratch / "manifest_raw.json").read_text(encoding="utf-8"))
    if any(row["status"] == "no_epub" for row in manifest):
        stage(binary, scratch, "probe13")
        retry13.main(str(scratch), epub_dir)


def build(binary: str, out: Path) -> str:
    """Every step from an empty folder to a query hit; returns that hit."""
    scratch = prepare(out)
    epub_dir = str(out / "epub")
    stage(binary, scratch, "versions")
    download.main(str(scratch), epub_dir)
    recover_missing(binary, scratch, epub_dir)
    for name in ("extract", "pii_scan", "pii_scan2", "finalize"):
        stage(binary, scratch, name)
    index = str(out / "index.sqlite")
    stage(binary, scratch, "search", "build", str(out), "--out", index)
    hit = stage(binary, scratch, "search", "query", index, *SMOKE_QUERY, "--limit", "1", capture=True)
    if not hit.startswith("1. "):
        raise QuickError(f"the build holds no row for {' '.join(SMOKE_QUERY)!r}")
    return hit


def main(argv: Sequence[str]) -> int:
    parser = argparse.ArgumentParser(
        prog="python -m fadden quick",
        description="Build and index a preset of principal Acts, then print the MCP folder setting.",
    )
    parser.add_argument("--out", required=True, help="an absent or empty folder")
    args = parser.parse_args(argv)
    binary = rust_binary()
    if binary is None:
        print("error: quick needs the Rust fadden binary; build it with "
              "cargo build --locked --release", file=sys.stderr)
        return 1
    out = Path(args.out).resolve()
    started = time.monotonic()
    try:
        hit = build(binary, out)
    except (OSError, ValueError, RuntimeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(hit, end="")
    print(f"Built and indexed {len(PRESET)} Acts in {time.monotonic() - started:.0f} seconds.")
    print("Set this for the aus-accounting-mcp server, then call search_tax_legislation:")
    print(f"AUS_ACCOUNTING_CORPUS_ROOT={out}")
    return 0
