## What it produces

The 8 September 2026 extraction checks covered the original 946-file snapshot
and a refreshed 951-title inventory. The original files were retrieved on
3 and 4 August. Reusing them measures the parser changes against the same
snapshot; it does not establish September source currency. All copied EPUBs
and sidecars were checked against their original SHA-256 hashes.

| Measure | Published baseline | Original snapshot reparsed | Refreshed inventory |
|---|---:|---:|---:|
| Titles | 946 | 946 | 951 |
| Retrieval rows | 21,784 | 21,916 | 21,958 |
| Words | 6,041,512 | 6,377,327 | 6,387,381 |
| EPUB bytes | 37,495,335 | 37,495,335 | 37,717,652 |
| Rates-index entries | 4,149 | 4,159 | 4,161 |
| Titles in the rates index | 286 | 286 | 287 |
| Parse failures | 0 | 0 | 0 |

The reparse used the current `extract.py` and `rates.py` in a separate deployed
`build/` directory. It did not overwrite the original cache or replace the
historical `fadden/manifest_md.json` in this repository. The collection counts
remain 175 Acts, 675 legislative instruments and 96 notifiable instruments.

For the refreshed inventory, discovery and version resolution completed for
951 titles without lookup failures. Downloads reused 922 matching cached files
and retrieved 17 new or updated documents. Twelve current versions had no
published document; `probe13` and `retry13` recovered earlier documents and
marked them `version_is_current: false`. None remained missing after recovery.
The refreshed collection contains 175 Acts, 677 legislative instruments and
99 notifiable instruments. Its totals reflect source changes as well as parser
corrections, so they are not a measure of the parser changes alone.

In both runs, the first privacy scan found 169 flagged rows across 12 titles. The second scan
exited with failure: 2,043 phone-shaped matches and 4 TFN-shaped matches had
no allowlist entry. These are scanner matches, not confirmed identifiers. There
were no additional name-and-registration-number rows outside the 12 flagged
titles. Finalisation and redistribution verification were not run after that
failure; this is extraction evidence, not a verified distribution. No corpus
text or contact identifiers are included in this evidence update.

A fresh-directory run also exposed a circular dependency in the documented
pipeline: `pii_scan.py` read `sources.json`, which the later `finalize.py` stage
creates. The scan now reads the extraction manifest, so it can run before
finalisation. The regression test exercises that order without a `sources.json`.

Historical full run, 4 August 2026:

| | |
|---|---|
| Titles | 946: 175 Acts, 675 legislative instruments, 96 notifiable instruments |
| Retrieval rows | 21,784 |
| Words | 6,041,512 |
| EPUB downloaded | 37.5 MB |
| Rates index | 4,149 entries across 286 titles |
| Download failures | 0 |
| Parse failures | 0 |

That historical run predates the volume-gate fix in `extract.py` and the
table-stack fix after it, and so does the `manifest_md.json` shipped beside it.
The 8 September reparse above includes these corrections:

- the volume gate, which silently dropped 5 volumes of F2025L00281
- the table stack, which let a table nested inside a cell discard the rows its
  enclosing table had already parsed. Four titles carry nested tables
  (F2005B01198, F2005L00211, F2005L01901, F2026L00716) and between them
  recovered 84 text fragments; the other 942 titles parse identically;
- the pre-section row. Text ahead of a document's first section reached the
  markdown but had no row to hold it, so it never reached `sections.jsonl`,
  which is the file retrieval reads. 511 of the 946 titles gain one
  `Introductory material` row each, carrying material such as the long title,
  the assent note and the enacting words. No title loses retrievable text;
- the pre-body table gate, which dropped a table or figure sitting between two
  pre-body paragraphs while keeping the prose either side. This changes the
  markdown for one title.

See [BUILD.md](../BUILD.md).

Layout under the corpus root:

```
epub/<register_id>.epub              the file exactly as the Register served it
markdown/<register_id>/<id>.md       full text, YAML front matter
markdown/<register_id>/sections.jsonl one row per section, ready for RAG
markdown/<register_id>/endnotes.md   amendment history, kept out of the sections
rates/rates.jsonl, rates/RATES.md    derived rates and thresholds index
sources.json, INDEX.md, README.md, LICENCE-NOTICE.md
```

Every JSONL row carries its own register id, collection, compilation number,
compilation date, section, source URL, licence and attribution, because rows
travel independently of the file they came from.

## Retrieval baseline on real law

Measured on 5 November 2026 over `tests/fixtures/retrieval_real_law.tsv`: 46 queries
written before any run, 32 in base form and 14 with inflected word forms, each naming
the section it should reach. An agent wrote them from the Acts' published section
numbers; no person has graded them. Every expected section is in the index below, on
one row, and its Register heading was read against its question. A hit is that section
of that title in the first 5 or 10 results.

| Ranker | Queries | Hit at 1 | Hit at 5 | Hit at 10 | Mean reciprocal rank | No results |
|---|---:|---:|---:|---:|---:|---:|
| `fadden search query`, base | 32 | 8 | 21 | 24 | 0.400 | 0 |
| `fadden search query`, inflected | 14 | 1 | 1 | 1 | 0.071 | 1 |
| MCP tier ranker, base | 32 | 11 | 17 | 22 | 0.422 | 0 |
| MCP tier ranker, inflected | 14 | 1 | 3 | 3 | 0.121 | 0 |

The MCP tier ranker is `search_sections` in `apps/aus-accounting-mcp/aus_accounting_mcp/corpus.py`
of australian-accounting at `6462edc`. Rerun the first 2 rows with
`FADDEN_REFERENCE_INDEX=<index> cargo test --locked --test fadden_search reference_questions_on_a_real_index -- --ignored --nocapture`.
These counts describe this question set over these 6 titles, not retrieval over the
whole corpus or legal correctness. The inflected rows show that neither ranker matches
another form of a word: the MCP ranker's substring match reaches some forms, and only
that way.

The index holds 6 titles, 8,096 rows and 28,438 references, built with `fadden search
build` from `sources.json` SHA-256 `43823bbd8b9ab8ab7128e2a4fcfd8b3b628ef4e1198550a8dbd54920422bfd29`:

| Register ID | Title | Compilation | Date | EPUB SHA-256 |
|---|---|---:|---|---|
| C2004A05138 | *Income Tax Assessment Act 1997* | 268 | 1 October 2026 | `05b7d33fbbfba411672c0c9341d282d14a3f9517bcfc2e6d347c349f47ea92ba` |
| C1936A00027 | *Income Tax Assessment Act 1936* | 192 | 1 July 2026 | `6b9686b4bdc77cd2c10816dcb086767d557b2a84fb3b80080ce2be3067fedb41` |
| C1953A00001 | *Taxation Administration Act 1953* | 227 | 1 October 2026 | `7d80b65a08cdc28f63fa81634e46ad4c0572d45693bc86c4ddb7d06e1aa5894c` |
| C2004A00446 | *A New Tax System (Goods and Services Tax) Act 1999* | 96 | 1 January 2026 | `a7e144d6139becf287ec49b86dbc9f7d07eb2de69a72b1e7ab3ad07deca52cb5` |
| C2004A03280 | *Fringe Benefits Tax Assessment Act 1986* | 97 | 1 July 2026 | `21ee9133acc6dbc48393dfb4ea334324d348c1b6f886d84e347c984dd899f57c` |
| F2025L01097 | *Income Tax Assessment (Effective Life of Depreciating Assets) Determination 2025* | 0 | 15 September 2025 | `aecb8c2f898f20a1556929c231564ebc18f53ebbc317395c5880fadab2c357ba` |
