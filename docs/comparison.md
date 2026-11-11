# Corpus and research tool comparisons

The [Open Australian Legal Corpus](https://huggingface.co/datasets/isaacus/open-australian-legal-corpus)
(Isaacus, CC BY 4.0) is the best-known open dataset of Australian legislation.
Use it when you need a ready-made, multi-jurisdiction dataset of whole
documents. Use this builder when you need Commonwealth tax law cut into
sections, with a compilation and in-force status on every row, and a way to
notice when the law changes.

Facts about the Open Australian Legal Corpus below are from its dataset card,
version 7.1.0, read on 24 September 2026.

| | Open Australian Legal Corpus | This builder |
| --- | --- | --- |
| What you get | A published dataset, loadable directly | Code; you build the corpus yourself (`python -m fadden`) |
| Coverage | 9 sources covering the Commonwealth, 5 states and Norfolk Island, including court decisions; 4,818 Commonwealth Acts and 27,538 Commonwealth instruments from the Federal Register | Commonwealth Acts and instruments whose titles match the tax keywords in [scope](scope.md) (946 titles in the dated build), plus named ATO rulings through the `rulings` stage |
| Unit | One record per document, with its whole text | Legislation: one row per section, with `row_id`, `section`, `heading` and `container`; `granularity` is `table_block` or `whole_act` where a title cannot be split into sections. Rulings: one row per numbered paragraph, with its own fields ([rulings](rulings.md)) |
| Version | Latest known version of each document; the card gives the collection date as 10 March 2025 | Legislation: the in-force compilation at build time, or the last one the Register published where the in-force version has none (`version_is_current: false`); `compilation_number`, `compilation_date` and `version_is_current` are on every row. Rulings: `fetched_on` only; no compilation status |
| Change tracking | New dataset versions | `check_current` compares the build with the Register; the `tax-radar-au` queue raises changes for review |
| Personal data | Not compared; see its card | 12 titles naming private individuals are excluded from distribution; organisational contacts need a reviewed allowlist entry ([scope](scope.md#what-this-deliberately-does-not-ship)) |
| Provenance | `url`, `when_scraped` and `version_id` per document | Legislation: Register page and source URL per row, a `sources.json` inventory, and live-capture evidence bundles with content digests ([evidence export](evidence-export.md)). Rulings: `source_url`, `source_sha256` and `licence_basis` per row |

Neither is the authorised text of the law. Check the Register's authorised PDF
before relying on a provision.

## Why there is no export in its format

Its records hold one document's whole text. Converting legislation rows into
that shape would drop the section, compilation and in-force fields that are the
reason to use this builder, and it would open a second distribution route that
also has to carry the personal-data exclusions above. If you need both, load
them separately.

## Related Australian legal-data projects

These projects overlap part of the same ground. The facts below are from each
project's README and GitHub metadata, read on 29 September 2026; none of them
was installed or run for this comparison.

The AsAt row was checked against its source at
[`34bfd63`](https://github.com/Aldiharley/asat/tree/34bfd63a220c3ba55ba82d27188870fb8c586543)
on 5 October 2026. Its legislation fixtures use the parent Act's commencement
for `coverage_from` because they lack provision commencement dates, except for
72 sections with their own date (24 in Division 7A and 48 in Division 355); the
separate application interval is not modelled. Retrieval for an income year
therefore does not establish that a provision's text applied in that year.

The same commit, still the default branch's head on 6 November 2026, was reread
that day through the GitHub REST API. Its README calls it a working research
prototype. Its `LEGISLATION-MANIFEST.tsv` lists 16 volumes of 6 Acts: 7 of the
ITAA 1997, 3 of the ITAA 1936, 2 of the GST Act, 2 of the SIS Act, 1 of the TAA 1953
and 1 of the *Income Tax (Transitional Provisions) Act 1997*. Each volume's URL
names one compilation, dated 1 January or 1 July 2026. So a past income year may
be served 2026 text unless something outside that manifest holds earlier text;
this comparison ran nothing to test that. No retrieval recall result was identified
in that README or in `eval_runner.py`.

The australian-law-mcp, australia-law and australia-instruments rows are from
each project's README, LICENSE file, releases and default-branch commit list,
and australia-instruments' `corpus.json`, read on 4 November 2026. None was
installed or run.

| Project | What it is | Where it differs from this builder |
| --- | --- | --- |
| [gunba/ato-mcp](https://github.com/gunba/ato-mcp) (MIT, v0.16.2) | A local MCP server over a pre-built download of about 158,000 ATO legal database documents (about 467,000 chunks), with hybrid lexical and vector search, statutory definition lookup and live fetch for documents it does not carry | It serves the ATO's own publication of legislation, cases, rulings and guidance as a ready-made search index. This builder produces Register compilations cut into sections with compilation status, and it tracks change. Use it for ATO guidance retrieval; use this builder when a row must name its compilation |
| [Aldiharley/asat](https://github.com/Aldiharley/asat) (AGPL-3.0) | Australian tax-law retrieval with temporal filters and quotation checks against retrieved source text. Its legislation coverage can extend back to the parent Act's commencement, or a section's own date for the 72 sections noted above, which may include years before the quoted provision applied | It answers questions; this builder produces material. Its fixtures do not establish provision commencement or application dates. This builder's [`as_at` stage](scope.md#dates-and-past-income-years) reports compilation windows and their limits, and [`search --as-at`](scope.md#dates-in-search) refuses rows from a title outside its window. Neither project establishes historical provision applicability. `quote_in_text` follows AsAt's quotation-checking approach with NFKC normalisation; a matching quotation alone establishes neither the correct authority nor its relevance |
| [matematicsolutions/au-eli-mcp](https://github.com/matematicsolutions/au-eli-mcp) (Apache-2.0, v0.3.3) | An MCP server that searches Commonwealth Acts on the Register by title and fetches current consolidated text, with a citation and a coverage statement on every response | Acts only and fetched live, so there is no local corpus, no instruments, no sections and no change review. Its `au_coverage` tool, which declares what it does not cover, is a good pattern for any consumer of this corpus |
| [cchew/lex-au](https://github.com/cchew/lex-au) (MIT, v0.10.2) and [cchew/lex-au-graph](https://github.com/cchew/lex-au-graph) (MIT, v0.13.1) | lex-au converts Register DOCX into Akoma Ntoso XML with section-level identifiers and publishes 3,076 Acts and 2 regulations as a CC BY 4.0 dataset; lex-au-graph builds a cross-reference graph over it for definition resolution | Every Commonwealth Act rather than a tax selection, in a legal XML standard, and the graph follows defined terms across Acts, which flat section rows cannot do. It does not carry this builder's tax instruments, ATO rulings or change-review queue |
| [Danielkgr/legislation-monitor](https://github.com/Danielkgr/legislation-monitor) (MIT) | A local web app that stores Commonwealth and Victorian Acts, re-fetches them on request and shows a side-by-side diff when the text changes, mapped to the Parts and sections it touches | It adds Victorian legislation. It works on text hashes, where `check_current` and `tax-radar-au` work on Register compilation identity and keep review evidence; `compare_builds` lists the changed sections between two builds of this corpus |
| [russellbrenner/jurisd](https://github.com/russellbrenner/jurisd) (Apache-2.0, 0.5.0 at [`a8d86f0`](https://github.com/russellbrenner/jurisd/tree/a8d86f0)) | A local-first AU and NZ legal research workbench and MCP server: offline legislation and decision modules with a citation graph, live AustLII retrieval and AGLC4 citation formatting | It answers research questions across every jurisdiction, including case law. This builder produces a provenance-bound Commonwealth tax corpus and holds no decisions |
| [ChangkeunJ/australian-law-mcp](https://github.com/ChangkeunJ/australian-law-mcp) (MIT, release v0.1.5, last commit [`c53e3e9`](https://github.com/ChangkeunJ/australian-law-mcp/tree/c53e3e95d21fa1681e3509ad4e558070581e683f) on 26 October 2026) | An MCP server, run with `npx`, that reads the Register API live: title search, the current or point-in-time text of a title or section, citation checks, amendment status, a comparison between 2 dates and full-text search. Commonwealth Acts and legislative instruments only; no state law, case law or ATO rulings | Every answer is fetched live from the Register, so there is no local corpus, and it covers every Commonwealth title rather than a tax selection. Its `verify_citations` checks that a cited Act and section exist on the Register; `refs` here resolves citations only between titles the index holds, and `search cite-check` checks a draft's citations offline against that one build and grades a quotation in each citation's sentence. A resolved citation shows the build holds that provision, and a graded quotation shows whether its words are in that row. Neither shows the provision says what the draft claims of it, applies to the facts or was the law at any date |
| [lexifina/australia-law](https://github.com/lexifina/australia-law) (legislation text CC BY 4.0, last commit [`bcfb2fa`](https://github.com/lexifina/australia-law/tree/bcfb2fa4f29eb67416ac853841abe46d6f06855d) dated 29 October 2026) | Commonwealth Acts as an offline git history: the Constitution, every Act in force and the repealed principal Acts with a registered compilation, one file per provision and one commit per amending Act on each day it took effect, credited to that Act. Its README calls the record partial: 1,230 of the 5,338 titles it will hold | Acts of every subject and no legislative instruments. `git log` and `git blame` give each provision's history across compilations, which this builder's per-build rows do not. Commits are dated to the day a change took effect, and its README says hashes and tags change between rebuilds. Its LICENSE says its scripts and documentation 'are provided for Lexifina's use' |
| [drfinlaymccall/australia-instruments](https://github.com/drfinlaymccall/australia-instruments) (text CC BY 4.0, code Apache-2.0, last commit [`226581e`](https://github.com/drfinlaymccall/australia-instruments/tree/226581eddb7be682a980e5fdd61458ac1f72d565) on 2 November 2026) | A named list of Commonwealth legislative instruments on foreign investment screening, merger notification and takeovers, as an offline git history with one commit per amending instrument on the day it took effect, and a local stdio MCP server that reads the clone without network calls | Built for M&A diligence. Its `corpus.json` lists 5 instruments: the Foreign Acquisitions and Takeovers Regulation 2015, 2 merger notification determinations, an ASIC takeover bids instrument and the Corporations Regulations 2001, none of which this corpus holds. It records each provision's history across compilations and compares every compilation word for word with the Register's Word document; this builder keeps each title's compilation in force at build time as section rows and checks only section identifiers against the Word rendition, for a fixed sample of titles (`witness`) |

## Commercial research tools

The vendor pages below were read on 5 November 2026, and those for Ask Nugget
and TimeBase on 4 November 2026. They describe advertised capabilities; no
product was used in a trial or tested against this corpus.

| Product and primary source | Advertised capability | Comparison with this builder |
| --- | --- | --- |
| [CCH iKnowConnect](https://www.wolterskluwer.com/en-au/solutions/cch-iknowconnect) | Research across legislation, cases, editorial commentary and practical tools; source-linked AI answers and daily or weekly news alerts | It provides a research service with editorial material. This builder supplies Commonwealth tax source rows and a change-review queue. Its provenance fields support source review, but do not supply CCH's commentary or establish the practical effect of a change. |
| [Lawg](https://lawg.ai/) | Structured tax answers with references to Commonwealth, state and territory legislation, ATO rulings and guidance and case law, with client documents analysed and turned into memos and letters | It answers research questions across sources. This builder holds no case law and generates no answer. A consumer can use its section and paragraph rows to check quotations, subject to the recorded coverage and date limits. |
| [Ask Nugget](https://help.taxnuggetsacademy.com.au/article/110-ask-nugget-knowledge-base) (Tax Nuggets) | AI tax research answers over Commonwealth tax Acts consolidated from the latest Register compilation, ATO public rulings and guidance, published edited advice and case law with decision impact statements. Its knowledge base page (updated 29 September 2026) lists draft and proposed law (including announced but unenacted measures), state taxes and international tax as coming soon, and withdrawn ATO material as out of scope | It answers research questions from the latest compilation. This builder generates no answer and holds no case law or edited advice. Each section row carries its compilation, so a consumer can tell when a quoted passage's compilation does not cover the question date, though not what the law was on that date (cases 1 and 2 below). |
| [TimeBase](https://www.thomsonreuters.com.au/en-au/products/timebase.html) (Thomson Reuters, which [completed its acquisition](https://www.thomsonreuters.com.au/en-au/press-releases/2025/july/thomson-reuters-completes-acquisition-of-timebase.html) on 2 July 2025) | 130,000+ legislative items across the 9 Australian jurisdictions and New Zealand, including Bills; [Point-in-Time](https://www.thomsonreuters.com.au/en-au/products/timebase/point-in-time.html) consolidation at any past, present or future date, red-line comparison of versions of a provision, and Bill-tracking alerts up to twice a day | It covers more jurisdictions, more history and Bills. This builder holds Commonwealth tax law at the in-force compilation only: it does not consolidate an amendment before it commences, and its radar queue raises a registered change for a person to assess (case 6 below). |
| [Australian Tax MCP](https://ato-mcp.com.au/about) ([william-laverty/ato-mcp](https://github.com/william-laverty/ato-mcp): AGPL-3.0 client, proprietary hosted platform and corpus) | A hosted MCP connector for Claude, ChatGPT, Cursor and Claude Code over 34,564 documents: ATO guidance, the ITAA 1997, the ITAA 1936, the GST Act and 4,937 public rulings, with withdrawn rulings flagged. It lists 6,468 sections, 2,310 statutory definitions and 64,217 cross-references, refreshes monthly, runs fixed calculations, and its README calls the legislation point-in-time aware. Unrelated to `gunba/ato-mcp` above | It puts cited answers in an agent today, with ATO guidance this builder does not hold. It names 3 Acts; this builder takes Commonwealth tax Acts and instruments, each row carrying its compilation. Its corpus is hosted and proprietary, where you build and keep this builder's rows yourself, and `tax-radar-au` raises changes for review. |
| [Nylon connector](https://www.usenylon.com/au/mcp) | An MCP connector for Claude, ChatGPT, Microsoft Copilot, Claude Code and Gemini CLI into Nylon's library of Commonwealth, state and territory legislation, ATO rulings and guidance and case law, included with its Adviser and Elite seats. The assistant searches and reads the documents and links each citation to the document in Nylon | The closest vendor to an agent route: a sign-in and a URL give an assistant more sources than this builder holds, including state law and cases. This builder needs a local build, holds Commonwealth law only, and records the compilation behind every row. |
| [Cassandra](https://www.cassandraresearch.com/) | Tax and legal research drafts across 23 jurisdictions, with linked authorities, 32 Australian engines and 34 Australian specialist tools. Tax products are restricted to registered agents, Australian legal practitioners and other approved professionals | It produces a draft for professional review. This builder generates no answer and covers one jurisdiction. |
| [Praxio AI](https://www.praxio-ai.com.au/) | Cited Australian tax research from a client scenario, grounded in legislation and ATO guidance, with clarifying questions, a client-ready draft and a saved history | It runs from client scenario to draft. This builder stops at source rows and a review queue, which a consumer can use to check a citation against a compilation. |
| [Tax Guru AI](https://www.tax-guru.ai/) | Source-linked tax research across 17 specialised modes over a proprietary database of expert-verified articles; the page gives both 20,000+ and 18,000+ articles | It answers from curated articles. This builder holds no commentary, and its rows are derived from the Register's reading view, not selected or verified by experts. |

### Cases for evaluating a research consumer

Run these cases with fabricated questions and retained public source text.
Record the source identifier, compilation or retrieval date, exact passage,
question date and coverage limitation beside each result. These are proposed
evaluation cases; no commercial product has passed them in this comparison.
This builder's own commands answer cases 1, 2 and 4 on fabricated text in
`tests/fadden_consumer_case.rs`, which runs on every test run; cases 3, 5 and 6
are not run there.

1. A quotation is absent from the cited row. The consumer must reject the
   quotation even if the source title and citation look plausible.
2. A quotation matches a row, but the question concerns a date outside its
   compilation window. Preserve the quotation result and the date limitation
   separately. `search cite-check --as-at` reports the quotation's grade and the
   title's status on the date as separate fields; neither establishes historical
   provision applicability.
3. The question needs a court decision, state legislation or editorial
   commentary. Declare that material outside this builder's coverage instead
   of presenting a Commonwealth section as a complete answer.
4. An ATO ruling paragraph is cited. Show its source URL and retrieval evidence;
   do not assign it legislation compilation status.
5. A change appears in `compare_builds` or the radar queue. Link the changed
   source and retain human review of its practical effect before updating
   downstream advice or workflows.
6. An amending Act is registered but has not commenced at the question date.
   Answer from the text in force at that date, and name the amendment and its
   commencement as a pending change rather than quoting the amended text as
   current law.
