## Checking staleness

    python check_current.py
    python check_current.py Act                    # one collection only

It asks the Register API for each title's current compilation and reports four
things separately: superseded compilations, titles whose in-force version has no
published compilation, titles no longer in force, and lookups that failed.
Read-only, about 1 minute for everything.

No run's counts are repeated here: it is a separate command, run after this
README is written, and its report goes to stdout. Its four buckets are not
interchangeable. The 0 titles listed in `sources.json` under
`titles_not_current_version` belong to the second bucket, not the first: their
in-force version has no published compilation, so there is no newer document to
re-download and the path built from that date answers 404.

## Build record

`build/build-record.json` records this finalisation: when it ran, the `fadden`
program that ran it (its version and SHA-256), the size and SHA-256 of every
file it read, and the checker files copied beside this README. What the program
cannot know, such as the source revision it was built from, is null with a
reason rather than guessed. The record covers finalisation and its inputs, not
the whole build. Earlier corpora shipped the builder's Python scripts in
`build/`; the builder is now the `fadden` program in the source repository.

## Licence and attribution

Licensed [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). Full statement in LICENCE-NOTICE.md. Every JSONL row
carries its own attribution, because rows travel independently of this file.

The markdown is CHANGED content (converted from EPUB, contents pages removed,
endnotes separated), so it uses the Register's "Based on content from" wording.
The EPUBs are unchanged and use "Sourced from".

The Commonwealth Coat of Arms is not covered by CC BY. It is embedded in the
EPUBs; the markdown omits all images and keeps only their alt text.

