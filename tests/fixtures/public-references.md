# Official source reader references

`public-references.json` pins 2 complete official EPUBs for ITAA 1997 and the
independently identified s 8-1 anchor. Download each `source_url` to its listed
filename in a new directory. The test verifies the complete EPUB hash before
running the real extractor in separate temporary corpus roots:

```sh
TAX_PUBLIC_EPUB_DIR=/absolute/path/epubs cargo test --test public_references -- --ignored --nocapture
```

PowerShell uses `$env:TAX_PUBLIC_EPUB_DIR = 'C:/path/epubs'` before the same Cargo
command. No network request occurs in the test. Each version must retain its own
compilation date, number, current flag and source URL whilst the unchanged s 8-1
text agrees. Expected section/anchor and subsection structure came from the
official EPUB HTML, independently of this extractor. These 2 references qualify
source reading and section identification, not complete search recall or legal
interpretation. Search qualification still requires a finished corpus and the
normal PII/finalisation gates. The test does not manufacture a finished corpus.

The Register API observed on 1 November 2026 also had an unpublished future row
starting 1 December with null compilation/register IDs and `isCurrent=false`.
The pinned current flag describes the observation date, not perpetual currency.
Do not select a current version by greatest start date alone or regenerate these
hashes merely to accept changed downloads. Review a new reference explicitly.
