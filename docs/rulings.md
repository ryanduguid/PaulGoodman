# ATO Legal Database rulings stage

`python -m fadden rulings TARGETS.json --out NEW_DIR` fetches named ATO-authored
documents from the Legal Database and writes one JSONL row per paragraph. It is a
finding aid like the rest of the corpus: not the authorised text, not advice and not
a statement of the Commissioner's current view. Read the source document at its
`source_url` before relying on anything.

## Reuse basis

The ATO copyright notice at
<https://www.ato.gov.au/about-ato/using-our-website/copyright-notice> (last updated
27 August 2012, read 24 September 2026 and again, unchanged, on 8 November 2026) says:

> You are free to copy, adapt, modify, transmit and distribute this material as you
> wish (but not in any way that suggests the ATO or the Commonwealth endorses you or
> any of your services or products).

Rulings, practical compliance guidelines and decision impact statements print the
same sentence at their foot. Edited versions of private advice carry a legacy
`dc.Rights` link (`http` or `https`, `www.ato.gov.au` or `ato.gov.au`,
path `/content/corporate/about_this_site.htm`, fragment `copyright`). That address
returned HTTP 404 on 21 November 2026. For local capture, the stage requires the
printed notice or the exact legacy link, which it records as
`site-notice-via-dc-rights`. The distribution requires the notice on the
document itself (`licence_basis: document-notice`). Legislation and
court judgments in the same database have separate terms. The stage accepts the
following docid families.

| docid prefix | Family |
| --- | --- |
| `TXR` | Taxation Ruling |
| `TXD` | Taxation Determination |
| `GST` | GST ruling or determination |
| `GSD` | Goods and Services Tax Determination |
| `CLR` | Class Ruling |
| `PRR` | Product Ruling |
| `COG/LCR` | Law Companion Ruling |
| `COG` (other) | Practical Compliance Guideline |
| `PSR` | Law Administration Practice Statement |
| `LIT/ICD` | Decision Impact Statement |
| `EV` | Edited version of private advice |

`GST` holds the GST rulings and keeps the label the Python stage wrote; GST
determinations (GSTD) sit under `GSD`. `COG` holds both Law Companion Rulings and
Practical Compliance Guidelines, told apart by the `LCR` at the start of the docid's
second part. Runs captured before this labelling called a Law Companion Ruling a
Practical Compliance Guideline, and runs captured before 16 November 2026 labelled a
GST determination `GST determination`, where the Legal Database says `Goods and
Services Tax Determination`: capture it again, as `dist` does not publish a document
whose label differs from its docid's family. Drafts (`COD`, `DTR` and the like) are
not accepted.

Every run writes `LICENCE-NOTICE.md` with the notice and a non-endorsement statement.
Keep it with any copy of the rows.

## Scope and safety

- **Named targets only.** `TARGETS.json` is a JSON array of distinct docids, at most
  100 a run, written by hand or by `rulings_targets` from the ATO's feed
  ([below](#targets-from-the-atos-feed)). There is no discovery crawl. A larger
  harvest needs its own authorisation, a stated page cap and a purpose before the
  cap changes.
- **New directory every run.** `--out` must not exist. The stage never overwrites,
  repairs or deletes an earlier run, and keeps each page's exact bytes under `raw/`
  beside their SHA-256 in `manifest.json`.
- **Personal data fails closed.** Every field that reaches the output (title, type,
  date, headings, paragraph text) and all text in the document region (the `Law*`
  divs, or the edited-version area from its disclaimer on, including the contents
  table and footnotes) go through the corpus patterns in `src/fadden/pii.rs` (a name beside an 8-digit registration number,
  email addresses, phone numbers, tax file numbers). A match excludes the whole
  document, records only the kind of match, and makes the run exit 1. The stage has
  no allowlist yet, so a document that prints an ATO contact email (such as
  PCG 2024/1) is excluded until a person reviews it.
- **Fetch rules.** One request a second, the corpus user agent, at most 8 MiB a
  page, HTML only, and the final URL must be `https://www.ato.gov.au` on the default
  port. The page's `ato.reference.id` must equal the target, ignoring case and a
  trailing slash. The ATO site header and footer sit outside the document region
  and are not scanned: they carry the ATO's published phone numbers. An HTTP 429 or 5xx, a transport error or a dropped response
  is retried within the shared budget of 3 attempts, 6 seconds apart; any other HTTP error stops that
  document, and a failed document never stops the run.
- **Not committed, not published.** Like the rest of the corpus, run output stays
  out of git. `dist` packages a run for publication ([Publishing a run](#publishing-a-run));
  publishing it is a separate decision a person makes.

## Targets from the ATO's feed

`python -m fadden rulings_targets FEED --out TARGETS.json` writes a target list from
an ATO Legal Database RSS feed, such as 'ATO - All Rulings and Determinations' at
<https://www.ato.gov.au/law/view/rss?fileid=pbr_all.rss>. `FEED` is a saved copy or
a feed URL under `https://www.ato.gov.au/law/view/rss`, fetched as `rulings` fetches a
page (its user agent, at most 8 MiB and 3 attempts) and refused if it redirects
elsewhere or is not XML. Nothing else is fetched. `--out` must not exist.

Each item's `<link>` names a document as `.../law/view/document?docid=<docid>`. In
the feed's order, a docid becomes a target when its family is one `rulings` accepts,
until 100 are listed. The stage refuses, and prints with its reason:

- a draft, whose number carries a `D` after the year (`DTR/TR2025D1/...` is
  TR 2025/D1, `DPC/PCG2025D1/...` is PCG 2025/D1);
- any other family, such as the `TDP` item the feed held on 17 November 2026;
- a docid already listed, ignoring case;
- every target past the 100th.

An item whose link it cannot read (none, more than one, markup or an unknown entity
in it, or a link that is not a Legal Database document) is printed as malformed with
its number and reason, and makes the stage exit 1 after writing the rest. A feed
whose items cannot be told apart, that has an item tag with a namespace prefix (such
as `<atom:item>`, which the stage does not read) or that is not RSS writes nothing.

The list holds identifiers only, so nothing from the ATO's pages is copied into it;
the feed itself is ATO material under the notice above. On 17 November 2026 at
06:25 AEDT the feed (HTTP 200, `application/rss+xml`) held 8 items, from which the
stage listed 7 targets and refused the `TDP` item. Review the list before a run.

## Output

| File | Contents |
| --- | --- |
| `rulings.jsonl` | One row per paragraph (fields below) |
| `manifest.json` | Run date, targets, per-document record, exclusions and reasons, row count, notice |
| `raw/<docid>-<digest>.html` | The exact bytes each accepted page returned; the digest of the canonical docid keeps distinct ids from colliding |
| `LICENCE-NOTICE.md` | Reuse notice and non-endorsement statement |

### `rulings.jsonl` fields

| Field | Meaning |
| --- | --- |
| `docid` | Legal Database document id, as targeted |
| `family` | Document family from the table above |
| `title` | `DC.Title` (or `DC_TITLE`) meta value |
| `paragraph` | Paragraph number: the document's own, or 1..n for edited versions |
| `numbering` | `document` when the ATO printed the number; `sequential` when the stage numbered unnumbered paragraphs |
| `heading` | Nearest preceding heading |
| `text` | Paragraph text with footnote markers removed; list items and continuation blocks join the paragraph they follow |
| `source_url` | Final URL the page came from |
| `source_sha256` | SHA-256 of the raw page bytes |
| `fetched_on` | Run date (ISO 8601) |
| `licence_basis` | `document-notice` or `site-notice-via-dc-rights` |
| `withdrawal_status` | `NOTICE_PRESENT` when extracted paragraphs follow a Notice of Withdrawal heading; otherwise `UNKNOWN` |
| `withdrawal_notice` | Extracted notice text, or an empty string when none was extracted |

The manifest's document record carries the same withdrawal fields as its rows.
They describe the page captured on `fetched_on`, identified by `source_sha256`.
`UNKNOWN` does not establish that a ruling remains current. A notice may preserve
reliance for an earlier period, so check its full effect and the relevant period
on the live ATO Legal Database before reliance. Older captures without these
fields carry no withdrawal observation.

A run prints each captured notice beside its docid, with that warning. Rows with a
notice stay in `rulings.jsonl`; `authorities` leaves them out of the closed list and
says so. `search build --rulings` keeps each paragraph's withdrawal status beside its
section references, and `search refs` prints it with the warning above. The build
refuses a run whose `rulings.jsonl` differs from its `manifest.json` in row count,
documents or paragraphs per document, or in a row's title, source URL, capture date or
withdrawal status against its document's record, and refuses a capture date that is not
an ISO date. It prints how many distinct documents the runs excluded and no run holds.

### How paragraphs are found

- Rulings and guidelines: blocks inside `div#LawBody` keyed by `P<n>` anchors,
  including a lettered notice anchor such as `PA`, with headings from `H<n>`
  anchors or a Notice of Withdrawal heading. The table of contents, footnotes and everything after
  the copyright anchor are skipped.
- Decision impact statements: the same body, but numbered in the text (`1. ...`)
  without anchors. Printed numbers are used only when a page has no numbered `P`
  anchors; a lettered withdrawal notice does not suppress them.
- Edited versions of private advice: the paragraphs after the `ev_disclaimer` box,
  ending at the "Back to top" bar; a paragraph whose text is all bold is a heading.

## Publishing a run

`dist` publishes every run it finds under the corpus's `rulings/` folder, beside the
legislation: write one there with `rulings TARGETS.json --out <corpus>/rulings/<run>`,
naming the run with letters, digits, `.`, `_` and `-` (at most 120 characters, as the
`aus-accounting-mcp` rulings reader requires). A corpus without `rulings/` publishes
exactly as before.

What a published run holds:

| Path | Contents |
| --- | --- |
| `dist/rulings/<run>/rulings.jsonl` | The run's rows, byte for byte, less any document left out |
| `dist/rulings/<run>/manifest.json` | The run's manifest, with each document left out moved to `excluded` with its reason and `rows` recounted |
| `dist/rulings/<run>/LICENCE-NOTICE.md` | The stage's reuse notice, as the stage writes it; an edit to the run's copy is not carried |
| `dist/rulings/sources.json` | Each run's capture date, document and row counts, the SHA-256 of each of its 3 files and every document left out, by the stage or by `dist` (the manifest's `excluded`) |

The distribution's `LICENCE-NOTICE.md` gains an `ATO rulings` section that quotes the
notice, gives its address, says the rulings are not covered by CC BY 4.0 and that
neither the ATO nor the Commonwealth endorses the corpus. `REMOVED.md` gains an
`ATO rulings left out` table, with a row for each document the stage or `dist` left
out. Point `AUS_ACCOUNTING_RULINGS_ROOT` at `dist/rulings`. On
15 November 2026 `aus-accounting-mcp` 0.9.0 answered from a published run of 2 Taxation
Rulings and a Class Ruling. It refused a run holding a Law Companion Ruling and a GST
determination. Version 0.11.0 serves Law Companion Rulings and both family labels
for GST determinations. Filtering by either GST label finds both. This was
verified on 21 November 2026 using fabricated runs with the published package.

What is not published:

- **The raw pages.** `raw/` stays behind, as the EPUBs do: each page carries the ATO
  site's header and footer, which hold its published phone numbers and were never
  scanned. A manifest's `raw_file` names the copy the builder kept.
- **Anything the ATO notice does not cover.** A document is left out, and listed with
  its reason, when its docid is not one of the families above (legislation and court
  judgments from the Legal Database, which a hand-edited run could hold), its page
  did not print the ATO's reuse notice (the required `licence_basis` is
  `document-notice`), its family label differs from its docid's family (a Law
  Companion Ruling captured as a Practical Compliance Guideline), or its title,
  headings or text carry a copyright notice of their own (`©`, `(c) 2099`,
  `Copyright 2099` or `All rights reserved`, in any case). Third-party material quoted
  without such a mark is not detected.
- **Edited versions of private advice.** An `EV` document is left out and listed. Each
  describes one taxpayer's affairs, edited by the ATO to protect their privacy, and a
  copy in a released snapshot would not follow any later change the ATO makes to it.
- **Personal data.** Every kept row's decoded values and every published file go through
  the corpus patterns again. The contact allowlist is not consulted, even for a run
  named like a title it approves contacts for, so one match stops `dist`
  before anything is published. The patterns find a name only beside a registration
  number, so a representative named without one is not found; `docs/scope.md` lists the
  patterns' other limits.

`dist_verify` adds 7 checks when `dist/rulings/` exists, or when the licence notice
or `REMOVED.md` has a rulings section: only the listed runs (at least one) and their
3 files, their digests, the privacy patterns over every file and row, each run's
agreement with its manifest (as `search build --rulings` checks it, with the stage's
own notice and the run's `sources.json` entry rebuilt from its manifest and files),
the catalogue's source and reuse notice, each run listed once, every published
document's coverage by the notice (and that none is an edited version of private
advice), `REMOVED.md` holding each left-out document's row exactly, and the licence
notice's `ATO rulings` section.

The reuse terms were read again on 15 November 2026 at 08:34 AEDT at
<https://www.ato.gov.au/copyright>, which redirects to the notice above ('Last updated
27 August 2012', QC17043), unchanged. The Legal Database's own page,
<https://www.ato.gov.au/law/view?fid=helpabout>, read at 08:49 the same day, adds that
'some content, in particular the principal legislation, is subject to further
copyright that prohibits reproduction for sale or incorporation in any product
intended for sale', which is why legislation comes from the Register instead.

Running `dist` publishes nothing outside the corpus folder. Publishing a snapshot that
holds rulings is a release step a person authorises ([RELEASING.md](../RELEASING.md)).

## Smoke run, 24 September 2026

Four targets, run from a Windows checkout into a scratch directory (not committed):

| docid | Result |
| --- | --- |
| `TXR/TR20231/NAT/ATO/00001` | 170 paragraphs, `document-notice` |
| `LIT/ICD/M47/2025/00001` | 47 paragraphs, `document-notice` |
| `EV/1052479123827` | 74 paragraphs, `site-notice-via-dc-rights` |
| `COG/PCG20241/NAT/ATO/00001` | Excluded: email pattern |

## Publication smoke run, 15 November 2026

Six targets in 2 runs, fetched on Linux at the stage's limits (24 seconds for 6
requests), then published by `dist` beside the `corpus-2026-11-11` snapshot's
legislation and checked by `dist_verify`, which passed. Nothing was committed or
released.

| docid | Result |
| --- | --- |
| `TXR/TR20211/NAT/ATO/00001` | 86 paragraphs, Taxation Ruling |
| `TXR/TR20262/NAT/ATO/00001` | 149 paragraphs, Taxation Ruling |
| `CLR/CR202689/NAT/ATO/00001` | 35 paragraphs, Class Ruling |
| `COG/PCG20214/NAT/ATO/00001` | Excluded by the stage: response is not UTF-8 |
| `GSD/GSTD20201/NAT/ATO/00001` | 61 paragraphs, GST determination |
| `COG/LCR20212/NAT/ATO/00001` | 112 paragraphs, Law Companion Ruling |

The 5 documents' 443 rows published as 483,003 bytes under `dist/rulings/` (65,250
bytes as a gzip-compressed tar); their raw pages, left behind, came to 1,086,553 bytes.

PCG 2021/4's page, read again on 15 November 2026, declares UTF-8 (`<meta charset="utf-8">`,
with no charset in its `Content-Type`) but writes the `×` and `÷` in 26 image titles as
single Windows-1252 bytes, so it is not UTF-8 and the stage refused it. The stage now reads
each byte that is not part of a UTF-8 sequence as Windows-1252 and records how many in
the document's `windows_1252_bytes` in `manifest.json` (26 for PCG 2021/4, read again
on 16 November 2026), and prints a `transcoded` line for it. It still refuses a page
holding a byte Windows-1252 leaves undefined (0x81, 0x8D, 0x8F, 0x90 or 0x9D) or one
that opens with a UTF-16 byte order mark. `raw/` keeps the bytes as they came.
