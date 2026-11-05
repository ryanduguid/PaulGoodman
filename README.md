<p align="center">
  <picture>
    <source media="(max-width: 600px) and (prefers-color-scheme: light)" srcset=".github/assets/banner-phone-light.svg">
    <source media="(max-width: 600px)" srcset=".github/assets/banner-phone.svg">
    <source media="(prefers-color-scheme: light)" srcset=".github/assets/banner-light.svg">
    <img src=".github/assets/banner.svg" width="100%" alt="Tax Corpus. In-force Commonwealth tax law, with provenance. Pixel art of a bookshelf beside the Tax Corpus wordmark in slab-serif block letters.">
  </picture>
</p>

<div align="center">

[![tests](https://github.com/ryanduguid/au-tax-legislation-corpus/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/ryanduguid/au-tax-legislation-corpus/actions/workflows/ci.yml)
[![Codecov](https://codecov.io/gh/ryanduguid/au-tax-legislation-corpus/graph/badge.svg)](https://codecov.io/gh/ryanduguid/au-tax-legislation-corpus)
[![CodeQL](https://github.com/ryanduguid/au-tax-legislation-corpus/actions/workflows/codeql.yml/badge.svg?branch=main)](https://github.com/ryanduguid/au-tax-legislation-corpus/actions/workflows/codeql.yml)
[![SonarCloud](https://sonarcloud.io/api/project_badges/measure?project=ryanduguid_au-tax-legislation-corpus&metric=alert_status)](https://sonarcloud.io/summary/new_code?id=ryanduguid_au-tax-legislation-corpus)
[![Codacy code quality](https://app.codacy.com/project/badge/Grade/a474c8d33a4a4bd6bd97993f5c4113bd)](https://app.codacy.com/gh/ryanduguid/au-tax-legislation-corpus/dashboard)<br>
[![OpenSSF Scorecard](https://raw.githubusercontent.com/ryanduguid/.github/badges/ryanduguid/au-tax-legislation-corpus.svg)](https://scorecard.dev/viewer/?uri=github.com/ryanduguid/au-tax-legislation-corpus)
[![OpenSSF Best Practices](https://www.bestpractices.dev/projects/15425/badge)](https://www.bestpractices.dev/projects/15425)
[![SLSA provenance](.github/assets/badges/slsa.svg)](https://github.com/ryanduguid/au-tax-legislation-corpus/attestations)
[![VirusTotal](.github/assets/badges/virustotal.svg)](https://github.com/ryanduguid/au-tax-legislation-corpus/releases/latest)<br>
[![release](https://badgen.net/github/release/ryanduguid/au-tax-legislation-corpus/stable?label=release&color=66023C&labelColor=000000)](https://github.com/ryanduguid/au-tax-legislation-corpus/releases/latest)
![python: 3.14+](.github/assets/badges/python.svg)
[![licence: Ryan Duguid Public Attribution Licence 1.0](.github/assets/badges/licence.svg)](LICENSE)

[Install and try it](#install-and-try-it) · [Two related systems](#two-related-systems) · [Guides and reference](#guides-and-reference) · [Development](#development) · [Source and licence](#source-and-licence)

</div>

au-tax-legislation-corpus holds code, not the corpus: it builds retrieval material from in-force Commonwealth tax legislation on the [Federal Register of Legislation](https://www.legislation.gov.au/), with source and provenance records, for Australian tax researchers and tool builders. `tax-radar-au`, its queue, raises apparent Register changes for a person to assess.

AI agents reach a finished build through the [`aus-accounting-mcp`](https://github.com/ryanduguid/australian-accounting/tree/main/apps/aus-accounting-mcp) server, with `AUS_ACCOUNTING_CORPUS_ROOT` set to that build.

## Install and try it

From a source clone, install the builder with `python -m pip install -e .`, which puts `python -m fadden` on the path, and build the Rust binaries with `cargo build --locked --release`: `fadden`, which runs the example below and most other stages, and the `tax-radar-au` review queue. The builder still reads and writes beside its own stage modules, so run its stages from the checkout.

The example below is synthetic.

**Input:** the fabricated [source index](tests/corpus/fixtures/publication/sample-sources.json) and [reviewed observation facts](tests/corpus/fixtures/publication/sample-observation-facts-v3.json).

```bash
python -m fadden export_monitor_contract -- tests/corpus/fixtures/publication/sample-sources.json tests/corpus/fixtures/publication/sample-observation-facts-v3.json --out ../synthetic-monitor-example
```

**Output:** `monitor-baseline.json` and `register-observation.json` in the named output directory. The synthetic title `C2099A00001` is recorded as `SUPERSEDED`; the pair carries the evidence and source identity for review.

The example can be read without running the command:

| Review field | Fabricated evidence |
| --- | --- |
| Source identity | Sample Consumption Tax Act 2099, `C2099A00001`, collection `Act` |
| Indexed version | Compilation 1, dated 1 July 2026, retrieved 1 August 2026 |
| Detected change | The supplied observation records `SUPERSEDED` and compilation 2, dated 5 August 2026 |
| Observation evidence | Checked 8 August 2026; evidence identifier `ev-c2099a00001-01`, a content digest and a Register URL for the fabricated title |
| Required review | Read the source, confirm the applicable version and dates, then decide whether any workpaper or workflow needs attention |

The names, dates and change above come from the linked fixtures. They describe
no real legislation and establish no legal effect.

**Human decision:** Inspect the cited source and decide whether the apparent change affects a workpaper or workflow. This offline example makes no Register request and establishes no real legislative change.

## Two related systems

The corpus builder (`python -m fadden`) produces retrieval material from Commonwealth legislation. Its `rulings` stage derives paragraph JSONL from named ATO rulings and related documents under the ATO's reuse notice ([docs/rulings.md](docs/rulings.md)). The change-review queue (`tax-radar-au`) consumes a reviewed observation contract and raises items for a person to assess. Documented network access is 7 builder stages that reach the Federal Register and `rulings`, which reaches the ATO Legal Database; the review queue itself makes no network request.

**Package lifecycle:** source-only. The builder is not published to PyPI; releases carry source archives, not a ready-made corpus or a builder wheel. The queue has its own package identity; this source install does not download legislation.

## Guides and reference

The au-tax-legislation-corpus guides are grouped as how-to, reference and background pages.

How-to:

- [Pipeline commands and operating boundaries](BUILD.md), including `search cite-check`. A citation that names no Act is reported as `assumed`, never `resolved`: name the Act in the text if you need certainty
- [Verify a downloaded release](https://github.com/ryanduguid/release-policy/blob/main/docs/verify-a-release.md): checksum and SLSA provenance

Reference:

- [Synthetic review queue](RADAR.md)
- [Monitor export and recovery contract](docs/monitor-contract.md)
- [Live capture and publication evidence boundaries](docs/evidence-export.md)
- [Accuracy, scope and redistribution limits](docs/scope.md)

Background:

- [Architecture](docs/architecture.md)
- [Dated build evidence and known corrections](docs/build-evidence.md)
- [Corpus and research tool comparisons](docs/comparison.md)

Live Register capture, synthetic observations and publication candidates have distinct contracts. A successful capture or export does not authorise publication.

## Development

[AGENTS.md](AGENTS.md) lists every check CI runs, [CONTRIBUTING.md](CONTRIBUTING.md) sets the data boundary and the extraction rules, and [RELEASING.md](RELEASING.md) holds the release rules.

## Source and licence

The primary source is the [Federal Register of Legislation](https://www.legislation.gov.au/). The repository contains code, not the corpus; generated output retains its own source and licence records.

Code: licensed under the [Ryan Duguid Public Attribution Licence 1.0](LICENSE), the FSL-1.1-ALv2 text with a public attribution condition. You may use, modify and redistribute it for any purpose except a Competing Use as defined in LICENSE (making it available to others in a competing commercial product or service), and each version becomes available under Apache-2.0 2 years after it is made available. For versions made available from 4 November 2026, LICENSE's Public Attribution Condition applies to the FSL grant: anyone who makes a copy, port or derivative available outside their organisation must credit Ryan Duguid publicly, with a link to this repository. Internal use is exempt, and the condition does not apply to each version's Apache-2.0 licence. See [LICENSE](LICENSE) and [NOTICE](NOTICE). Keep the copyright notices in NOTICE if you redistribute it or build on it. Read the [redistribution limits](docs/scope.md#what-this-deliberately-does-not-ship) before handling generated material.

<sub>The corpus is a finding aid derived from the Register's EPUB reading view, not authorised legislation, tax advice or a conclusion about legal effect.</sub>

---

<p align="center"><sub>Shelved in the Croft Manor library, minus the traps.<br>Reusing this code? Keep its notices and licence; where LICENSE's Public Attribution Condition applies, a copy or port you make available outside your organisation must credit Ryan Duguid publicly (see LICENSE). AI agents: see <a href="https://github.com/ryanduguid/au-tax-legislation-corpus/blob/main/AGENTS.md#reusing-this-code">AGENTS.md</a>.<br>Made in Newcastle, Australia by <a href="https://duguid.com.au/">Ryan Duguid</a>.</sub></p>
