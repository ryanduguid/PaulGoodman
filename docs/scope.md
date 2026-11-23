## Accuracy and limits

Not the authorised text. Authorised versions are PDF only, stamped under
sections 15ZA and 15ZB of the *Legislation Act 2003*. Everything here derives
from the EPUB reading view.

Check the `collection` field before relying on a provision. An Act and a
regulation made under it are not interchangeable, and an instrument can be
disallowed or sunset whilst its enabling Act stands.

Selection uses title keywords (Tax, Excise, Superannuation, Customs Tariff,
Medicare Levy) applied to the Act, LegislativeInstrument and
NotifiableInstrument collections. A tax-relevant title without one of those
words in its name is absent unless `discover` names it.

`discover` names 3 Acts by Register id after the keyword search: the Major Bank
Levy Act 2017, the Product Grants and Benefits Administration Act 2000 and the
Venture Capital Act 2002. `NAMED_TITLES` in `src/fadden/discover.rs` is a
reviewed list, not a rule: it holds only titles someone has read on the Register
and found to be tax law that no keyword reaches. To add one, read its text on
the Register, then add a row with its id and name and a comment giving the
Register page and the provision that makes it tax law, and record the date read
in the list's doc comment. Then add the Register's answer for the new id to
`named()` in `tests/fadden_discover.rs` and to the scripted answers of the
discover cases in `tests/golden`, and re-record those cases' outputs. `discover`
stops, writing nothing, if a listed title is no longer an in-force principal
Act, and prints a line asking for a row's removal when a keyword already matches
that title.

The Register's `contains(name,...)` also matches
former titles and substrings, which is how the Passenger Movement Charge Act
1978 arrived under its old name, the *Departure Tax Act 1978*, and how 2 dozen
`AD/ROTAX/...` airworthiness directives arrived because 'Rotax' contains 'tax'.
`sources.json` records `keywords_in_name` rather than filtering them out, since
any rule strict enough to drop Rotax also drops the Departure Tax Act.

Some titles have no published compilation for the version now in force, so they
carry the last compilation the Register holds, marked `version_is_current:
false` in the front matter and on every row. There is no newer document to
re-download: the URL built from the in-force date answers 404.

## Dates and past income years

Each row carries the window of the compilation it came from:
`compilation_date`, the start date of that compiled text; `superseded_from`,
the start of a later version the Register had published no compilation for, or
in an earlier compilation the start of the next version (otherwise null); and
`retrieved`, the day the build obtained it. A compilation
date is not necessarily the commencement or application date of every
provision or amendment in the compilation. A provision such as ITAA 1997 s 8-1
commenced decades before any current compilation of that Act, and its text in
an earlier year may have differed.

So a row cannot answer a question about an earlier income year on its own.
`python -m fadden as_at YYYY-MM-DD` classifies every title in the build's
manifest against a date: `captured` when the compilation the build holds is
the one its metadata shows for that date, `before_compilation`,
`superseded` or `after_retrieval` otherwise, each with the next step. It works
from the committed `fadden/manifest_md.json` unless `--manifest` names another,
and `--json` prints one object per title.

`captured` is not a finding that the text was the law on that date. A
compilation does not show application, saving or transitional provisions
elsewhere, modification by another law, or an amendment with retrospective
effect registered after the build was retrieved, and the Legislation Act 2003
does not require a new compilation for one. Read the Register's version
history and endnotes for any question about a past date.

### Dates in search

`search query` matches English word forms as well as the words typed, and
`--stem` matches word forms alone. Dated word-form search uses the same
capture-window restrictions below, including the withheld-row counts and
filtering before the result limit. Each JSON hit that only a word form matched
is marked (every hit, with `--stem`), and text output warns that word-form
matches are not quotation matches; `--exact` matches only the words typed. It does not extend compilation coverage or change quotation checking.

`search query`, `search define` and `search cite-check` take `--as-at YYYY-MM-DD` and apply the `as_at` status of each title to what they return. `query` and `define` return rows only from `captured` titles and refuse the others by name, with the status, the rows withheld and the next step. `cite-check` keeps every citation and adds its title's status. No row is dropped without a refusal, and a title's status is about its compilation window, not about the law on that date. With no rows to return, `query` and `define` refuse every title in scope that is not `captured`, so 0 rows with exit 0 means each title in scope was `captured`. `cite-check --title` refuses a named title that is not `captured`, whatever the draft cites.

| Status | Rows | What it covers | Next step |
| --- | --- | --- | --- |
| `captured` | Returned | The build holds the compilation for that date. It is not a finding that the text was the law then | Read the Register's version history and endnotes for a past date |
| `before_compilation` | Refused | The date is before this compilation's start, so the build holds no text for it | Read the Register's earlier compilations |
| `superseded` | Refused | A later version had started by that date, and the Register had no compilation for it when the build ran | A later version started; check the Register and application provisions |
| `after_retrieval` | Refused | The date is after the build retrieved this title, so the build cannot say what changed | Run `check_current` and read the Register |

A compilation can be superseded on the day it starts, since the Register ends a version on the day another starts on that same day, and `retry13` can fall back to one; its window holds no date, so it is never `captured`.

The limits above apply to every status. `--as-at` reads the window `compilation_date`, `superseded_from` and `retrieved`, so a date inside a window can still carry text amended with retrospective effect, and a provision's text can differ from what applied to that date. The index and the `as_at` stage hold one compilation of a title, the root's, so their refusal is the answer for any earlier date: no earlier text is in the index.

With `--json`, `query` and `define` print one object per line: each hit with an `as_at` object, then one `{"refused": true, ...}` per refused title with `register_id`, `act`, `matching_rows` and `as_at`, then `{"as_at_summary": ...}` with `date`, `rows`, `withheld_rows`, `titles` (a count per status) and `covers`. The `as_at` object holds `date`, `status`, `compilation_date`, `superseded_from`, `retrieved`, `covers` and `next_step`, which is null for `captured`. `cite-check --json` adds the same object to each citation, or null if the citation reached no title, then prints a refusal for a `--title` that is not `captured`.

The index stores `superseded_from` from schema 3, so an index built earlier is refused until `search build` runs again. A consumer that reads `sections.jsonl` instead of the index, as the MCP server does, can apply the same 4 statuses from each row's own `compilation_date`, `superseded_from`, `retrieved` and `version_is_current` fields, and should report the same field names. `version_is_current` is false for a title with no published compilation for the version in force.

### Earlier compilations

A build can also hold earlier compilations: `quick --in-force-on YYYY-MM-DD` ([BUILD.md](../BUILD.md#earlier-compilations)) adds each preset Act's compilation in force on that date. This is the contract a consumer reads:

- Each earlier compilation lives at `compilations/<name>/markdown/<register id>/sections.jsonl` under the corpus root, `<name>` being the date (`YYYY-MM-DD`) it was resolved for. A build without the option has no `compilations/` folder.
- Its rows have exactly the format of the root's `markdown/`, with `compilation_date` the version's start, `version_is_current` false and `superseded_from` the start of the next version. In the root that flag marks a title with no published compilation for the version in force; here it marks a compilation that a later one replaced.
- Each compilation is held once: the root holds the current one, and a date holds only compilations that neither the root nor an earlier date holds. The windows of one title therefore never overlap, so at most one covers a date.
- `row_id`s repeat across compilations of a title, so a row is identified by its folder as well as its `row_id`.

A `dist/` tree carries the same layout: `dist/compilations/<name>/markdown/` beside a `sources.json` naming each compilation's source, window, retrieval date and file digests, and the titles left out of that date.

The index, `as_at` and `search --as-at` read the root only. A consumer that reads `sections.jsonl` directly can answer a dated question from the compilation whose window covers the date. The limits of `captured` above apply to each compilation.

### Amendments a build cannot see

An amendment that has commenced but is not yet in a compilation shows on the
Register as a current version with no `registerId`. The Register API also sets
`hasUnincorporatedAmendments` on that version and
`hasCommencedUnincorporatedAmendments` on its title; on 5 November 2026 the same
253 in-force versions met both tests and none with a `registerId` carried the
flag. `check_current` counts such a title under no published compilation,
and a build marks it `version_is_current: false`. A build stores neither flag,
so `as_at`, which makes no request, cannot report an amendment that commenced
after the build was retrieved: run `check_current` for that. Neither command
reports amendments that have not commenced.

A consumer that quotes a row should accept a quotation only when it appears
in the row text. `quote_in_text` in the Rust crate's `fadden::as_at` module does that check after Unicode
NFKC normalisation of both, reading the Register's non-breaking hyphen as `-`.
NFKC also folds other compatibility forms, such as ligatures and full-width
letters, and compatibility punctuation such as a full-width comma, so a match
is close to literal rather than character for character. Case is not folded.
A match shows the quotation is
faithful to the row, not that the row is the law for the question.

## What this deliberately does not ship

The full corpus. It is about 150 MB (the EPUBs, derived markdown and rates,
measured on 27 September 2026), and this code rebuilds it in an afternoon. Three
parts of it should not be redistributed at all, and the `dist` stage exists to separate
them out:

1. **The EPUBs embed the Commonwealth Coat of Arms**, which is excluded from the
   Register's CC BY 4.0 grant, along with any third-party material the Register
   has not cleared. Stripping the image would also destroy the only thing that
   makes shipping EPUBs worthwhile, which is that they are byte-identical to
   what the Register served. The markdown and JSONL carry no image data at all, because
   the `extract` stage never emits any, so they need nothing removed.
2. **Twelve titles name private individuals.** The Tax Practitioners Board
   registers terminations and suspensions of tax and BAS agents as notifiable
   instruments: tables of named people with registration numbers and the
   provision breached, about 5,400 name mentions across 169 rows. Public on the
   Register as PDFs you read one at a time; shipping them as dataset rows makes
   them name-searchable at scale, which is a different act.
3. **Five accounting standards carry their own copyright notices.** AASB 112,
   2021-5 and 2023-2 reproduce IFRS Foundation material for personal,
   non-commercial use only, and AASB 1056 and 2023-4 need the AASB's written
   permission for any reproduction (each read at source on 27 September 2026).
   The CC BY 4.0 grant does not reach them, so the `dist` stage drops them whole and
   `dist_verify` fails any tree that carries one.

The `pii_scan` stage finds the second category without being told where to look. It
tests every row in all 946 titles for personal names appearing alongside agent
registration numbers, because a bare name test flags the whole corpus:
legislation names Ministers, Commissioners and litigants constantly, and the
registration number is what separates a disciplinary register from a statute.
The `pii_scan2` stage re-runs at a lower threshold and sweeps for emails, phone numbers
and 8- or nine-digit tax file numbers. On the source snapshot retrieved
2026-08-04, the JSONL scan finds 10 occurrences of 4 unique organisational
contacts across 3 titles: 2 government email addresses and 2 government
landlines. They are approved by a title-bound SHA-256 fingerprint in
`pii_contact_allowlist.json`; the policy does not store the identifiers
themselves. An allowlist entry is reserved for a manually reviewed
organisational contact published in that primary instrument; personal contacts
and TFNs are never allowlisted. Pattern false positives are fixed in
the shared patterns (`src/fadden/pii.rs`) with a regression test rather than approved as data. Every
exception is bound to the identifier kind, digest and Register id and must carry
a review reason.

The scan, the `dist` preflight and `dist_verify` all fail on a new or moved
contact identifier, and their diagnostics contain only Register ids and
truncated fingerprints. The publication gates require each title's exact
declared Markdown/JSONL file inventory and inspect every file in it, covering
the human-readable Markdown and endnotes as well as `sections.jsonl`. A private
name beside a registration number in any representation fails, as does an
unreadable file, binary control data, a nested path or an unexpected file.
`dist_verify` also reads each row's decoded strings and numbers, keys and
nested values included, so a JSON escape cannot hide a name, registration
number or contact.
`dist` also refuses a title with no rows, and a title name, kept or left out,
that pairs a person with a registration number or holds an unapproved contact.
The contact patterns read each text 8 ways: as written; as rendered, with HTML
character references decoded (such as `&#64;`, `&#x40;` or `&commat;` for `@`)
and then NFKC-normalised, which folds compatibility forms such as a full-width
`@` or full-width digits; each of those 2 with the characters a reader does not
see removed, those Unicode marks `Default_Ignorable_Code_Point` or as format
characters, such as a zero-width space, joiner or non-joiner, word joiner, byte
order mark, soft hyphen, direction mark, variation selector or tag character;
each of those 2 with controls other than white space removed as well, such as a
NUL a JSON escape decodes to; and the rendered text as it shows, twice, with
invisible characters removed and then with controls removed as well: in the
order the Unicode bidirectional algorithm displays it, so a contact written
backwards after a right-to-left override reads forwards, and with a hyphen,
non-breaking hyphen, figure dash, en dash or minus sign read as a hyphen-minus.
A contact any reading finds is reported. Where removing an invisible character
joins a contact to the letters or digits beside it, the joined contact a reader
sees is reported instead. The patterns do not find a contact broken by a
character that shows as a gap, such as a space or U+2800 BRAILLE PATTERN BLANK,
or one split across 2 strings: each string is read alone, as the Markdown shows
each field on its own line. Joining each of the 160,424 pairs of adjacent values
in the 9,439 rows of the 2026-11-11 corpus snapshot raised 306 more flags, all
phone-number matches on section numbers and row ids and none a contact (286
joined directly, 20 with a space between); the 1,111 rows in `tests/golden`
raised none, and joining adjacent rows' text raised none in either. The name
and registration number patterns read text as written.

For that validated source snapshot, the `dist` stage writes `dist/`: 934 titles and
21,596 rows, the shipped manifest's 946 titles and 21,784 rows less the 12
titles `pii_flagged.json` names. Those 12 titles hold 188 rows between them, of
which `pii_flagged.json` records 169 as naming private individuals. The `dist` stage
drops a flagged title whole rather than row by row, so all 188 of its rows leave
the distribution, not only the 169.
A `REMOVED.md` lists every exclusion and its Register link so the omission is
visible and reversible; an earlier compilation's folder lists its own in its
`sources.json`: the root's removals, the date's own `pii_scan` flags and the restrictive copyright notices all apply to it. `dist_verify` checks the result against its own
claims and exits non-zero if any check fails.

A `dist/` tree that `dist_verify` passes is the one generated output a release
may carry. It goes as a data asset under CC BY 4.0 with the Register's
attribution, on the conditions in [RELEASING.md](../RELEASING.md). The EPUBs,
the full corpus and every other generated tree stay out of releases.

## Source intake record

Each source class the builder reads has one row. A new source class needs its
row, checked at the source, before any stage reads it. The Register terms, at
<https://www.legislation.gov.au/terms-of-use>, and the copyright notice on ATO
Legal Database pages were read on 2 November 2026; the Queensland Legislation
copyright page on 24 November 2026.

| Question | Federal Register of Legislation (Acts, legislative and notifiable instruments) | ATO Legal Database (`rulings` stage) | Queensland Legislation (revenue Acts and regulations only) |
| --- | --- | --- | --- |
| What does it publish? | As-made versions and compilations, each with an authorised PDF and an EPUB reading view; the compilations `witness` samples also have a Word rendition, in 1 or more volumes | ATO-authored rulings, determinations, guidelines, practice statements, decision impact statements and edited private advice, as HTML pages; legislation and judgments in the same database are refused | In-force reprints of Queensland Acts and subordinate legislation, each as HTML, PDF and whole-title XML under the register's own ids (`act-2001-071`, `sl-2023-0104`) |
| What authority does the copy have? | None: authorised versions are the PDFs stamped under sections 15ZA and 15ZB of the *Legislation Act 2003*, and the corpus derives from the EPUB | None: a row is a finding aid, not the published document or the Commissioner's current view; read it at `source_url` | None: the XML carries the register's disclaimer and is not an authorised version; read the title on the register |
| How is it retrieved? | The Register API with ordered paging and title keyword selection, then the download endpoint (raw EPUB bytes or a base64 JSON envelope); see [architecture](architecture.md). `witness` reads Word volumes an operator fetches from the API's `documents` entity and keeps only their section identifiers; see BUILD.md | Named docids only, at most 100 a run, one request a second, HTML only, final URL on `https://www.ato.gov.au`; `rulings_targets` may read one Legal Database RSS feed (XML) to write that list and keeps only its docids; see [rulings](rulings.md) | No stage reads it yet. Intake is limited to named titles: the Duties, Land Tax and Payroll Tax Acts and their regulations, and the First Home Owner Grant and Other Home Owner Grants Act 2000, each as whole-title XML from `https://www.legislation.qld.gov.au/view/whole/xml/inforce/current/<id>`, one request at a time with a pause between them; robots.txt allows that path and disallows `/view/pdf/` and the compare views |
| May the text be republished? | Yes under CC BY 4.0, except the Commonwealth Coat of Arms and material otherwise noted, with the Register's attribution wording, which `extract` and `finalize` write. The `dist` stage drops the EPUBs, the 12 titles naming private individuals and the 5 accounting standards under their own notices (above) | Yes under the ATO copyright notice; the stage accepts a page only when it shows the notice or links to it, and writes `LICENCE-NOTICE.md` | Yes under CC BY 4.0, except the coat of arms, logos, trade marks and third-party material, keeping the copyright notice and using the register's changed-content attribution wording, which NOTICE records |
| May the metadata be republished? | Yes, under the same CC BY 4.0 grant as the text | Rows carry the docid, title, source URL and page hash under the same notice; keep `LICENCE-NOTICE.md` with them | Yes, under the same CC BY 4.0 grant |
| How are superseded versions handled? | Each row carries `compilation_date`, `superseded_from` and `retrieved`; a title with no published compilation for the version in force is marked `version_is_current: false`; `as_at` classifies titles against a date; live capture records repeal as `NO_LONGER_IN_FORCE` | `withdrawal_status` is `NOTICE_PRESENT` only when a Notice of Withdrawal was extracted, otherwise `UNKNOWN`; neither value establishes that a ruling is current | Each title carries the reprint's `publication.date` and `first.valid.date`; only the current in-force reprint is read |

### State and territory registers

No stage reads a state or territory register except as its column above
records. Each register's own copyright or terms page was read on 24 November
2026, with its robots.txt. A register is read only where its terms allow a
derived corpus to be republished with attribution and it serves a stable
machine-readable form; any other reading is the maintainer's decision, not a
builder default. CC BY 4.0 permits commercial use wherever it applies.

| Register | Terms | Copying, redistribution in a derived corpus, commercial use | Attribution | Machine access | Reading |
| --- | --- | --- | --- | --- | --- |
| NSW | <https://legislation.nsw.gov.au/copyright> | Not read | Not read | Every request, robots.txt included, answered with a Cloudflare challenge page (HTTP 403, `cf-mitigated: challenge`), also after a change of network exit | `unread` |
| Vic | <https://www.legislation.vic.gov.au/copyright> | Authorised versions are published for personal use only; no reproduction except under the *Copyright Act 1968* | None offered; enquiries to the Office of the Chief Parliamentary Counsel | robots.txt: `Crawl-delay: 2`, document files under `/sites/default/files/` disallowed | `allowed for personal or internal use only` |
| Qld | <https://www.legislation.qld.gov.au/copyright> | Yes, CC BY 4.0 'for any purpose, even commercially'; coat of arms, logos, trade marks and third-party material excluded | Keep the copyright notice; changed content: 'Based on content from the Queensland Legislation website at [full date of download]. For the latest information on Queensland Government legislation please go to https://www.legislation.qld.gov.au.' | robots.txt disallows `/view/pdf/`, the compare views and some named crawlers, sets no delay and lists a sitemap and a crawler feed; whole-title XML at `/view/whole/xml/inforce/current/<id>` | `allowed with attribution` |
| WA | <https://www.legislation.wa.gov.au/legislation/statutes.nsf/copyright.html> | Yes, CC BY 4.0 'for any purpose, even commercially'; coat of arms, trade marks, material not owned by the State, the Government Gazette and site design excluded; no suggestion of endorsement | Keep the copyright notice; changed content: 'Based on content from the Western Australian Legislation website at [full date of download]. For the latest information on Western Australian legislation, visit www.legislation.wa.gov.au.' | robots.txt absent (HTTP 404); machine-readable form not checked | `allowed with attribution` |
| SA | <https://www.legislation.sa.gov.au/copyright> | Yes, CC BY 4.0 'including for commercial purposes'; Piping Shrike, Royal Arms and other State emblems excluded | Keep the copyright notice; changed content: 'Based on content from the South Australian Legislation website at [full date of download]. For the latest information on South Australian Government legislation, please go to www.legislation.sa.gov.au/.', plus the disclaimer that the copy is not published under the *Legislation Revision and Publication Act 2002* | robots.txt: `Crawl-delay: 10`, `/search` disallowed; machine-readable form not checked | `allowed with attribution` |
| Tas | <https://www.legislation.tas.gov.au/copyrightanddisclaimer> | Yes, CC BY 4.0 'for any purpose, even commercially'; coat of arms, logos, trade marks and third-party material excluded | Keep the copyright notice; changed material: 'Based on material from the Tasmanian Legislation website at [full date of download]. For the latest information on Tasmanian Government legislation please go to www.legislation.tas.gov.au.' | robots.txt disallows `/admin/` only and lists a sitemap and a crawler feed; whole-title XML at `/view/whole/xml/inforce/current/<id>` | `allowed with attribution` |
| ACT | <https://www.legislation.act.gov.au/Static/Footerlinks/Copyright/copyright.html>, which defers to <https://www.justice.act.gov.au/copyright> | The policy grants CC BY 4.0 over 'content on this website', the Directorate's own site; whether that reaches the Register is not stated | '© Australian Capital Territory', a licence link and a statement of changes | robots.txt: `Crawl-delay: 10`; `/View/`, `/html/` and `/results` disallowed | `unclear` |
| NT | <https://legislation.nt.gov.au/en/Footer/Terms-of-Use> | A permission to publish legislation, revocable on reasonable notice, on conditions: not presented as an official version, no Territory arms, accurately reproduced in a context that does not mislead | None required | robots.txt blocks 2 named site-audit crawlers only | `unclear` |

The operative sentence of each:

- NSW: none read.
- Vic: 'Authorised electronic versions are published on this website under
  section 62 of the Interpretation of Legislation Act 1984 for personal use
  only.' and 'No part may be reproduced by any process except in accordance
  with the provisions of the Copyright Act 1968 of the Commonwealth.'
- Qld: 'Unless otherwise noted, all copyright material available on or through
  this website is licensed under a Creative Commons Attribution 4.0
  International licence (CC BY 4.0).'
- WA: 'With certain exceptions (listed below), the State of Western Australia's
  copyright material available on this website (Licensed Material) is licensed
  under a Creative Commons Attribution 4.0 International Licence (CC BY 4.0).'
- SA: '... the SA Content of this Website is licensed under the Creative Commons
  Attribution 4.0 International Licence (CC BY 4.0)'.
- Tas: 'Unless otherwise noted, all material provided on this website is
  licensed under a Creative Commons Attribution 4.0 International licence (CC
  BY 4.0).'
- ACT: 'Use of material on the ACT Legislation Register should be in accordance
  with the JACS Copyright Policy developed by the Justice and Community Safety
  Directorate.' The policy says 'Unless stated otherwise, the copyright of
  content on this website is owned by the Australian Capital Territory (ACT)
  and licensed under a Creative Commons Attribution 4.0 licence.'
- NT: 'The Northern Territory grants permission to any person to publish or
  deal with any legislation of the Northern Territory in accordance with the
  following conditions' and 'The Northern Territory reserves the right to
  revoke, vary or withdraw its permission on reasonable notice'.
