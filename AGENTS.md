# Agent instructions

This repository combines 2 deliberately separated systems: a corpus builder and a
synthetic change-review radar. Treat corpus outputs, synthetic monitor inputs, live
Register captures and publication candidates as different contracts.

- Synthetic monitor contracts project fabricated or separately reviewed observation
  facts and never query the Register. Live Register capture queries the official API,
  retains its bounded response bytes and emits a deliberately incompatible live contract.
- Never commit raw live captures, retained Register responses or generated corpus,
  evidence, distribution, demo or build output.
- Treat official capture and export destinations as immutable: use an absent destination,
  never overwrite, repair or delete an earlier output, and preserve the exact source bytes,
  content digests and cross-file provenance that support each result.
- The supported official live-evidence export boundary is Windows only. On every other
  platform it must fail closed before creating an output parent or staging directory.
- Live capture and local export are not publication authority. A human must explicitly
  authorise every tag, release, public development and deployment; `VERIFIED` evidence and
  passing checks do not authorise publication.
- The corpus is a finding aid derived from the EPUB reading view, not authorised legal
  text, tax advice or a conclusion about the practical effect of a change.

## Repository map

- [README.md](README.md) explains the 2 halves, corpus limits and high-level operation.
- [BUILD.md](BUILD.md) owns pipeline, monitor-export, live-capture and live-export details.
- [RADAR.md](RADAR.md) owns the synthetic review-queue contract and human-review boundary.
- [RELEASING.md](RELEASING.md) owns builder release preflight and immutable-release rules.
- [docs/scope.md](docs/scope.md) owns corpus limits and the source intake record. A new
  source class needs its row, checked at the source, before any stage reads it.
- The approved [Stage 3A capture design](docs/superpowers/specs/2026-08-29-live-register-capture-design.md)
  and [Stage 3B evidence design](docs/superpowers/specs/2026-08-29-authenticated-live-evidence-admission-design.md)
  own the trust, provenance and publication boundaries. Keep operational detail there
  instead of duplicating it here.

## CI gates

`ci.yml` runs the full locked pytest suite on Ubuntu for Python 3.14 and 3.15, then the
package build and the linters. Its Windows 3.14 matrix runs
`tests/corpus/test_live_evidence_bundle_export.py` and
`tests/corpus/test_live_evidence_publication_paths.py` under branch coverage so the
supported live-evidence export boundary is exercised. Codecov merges that report with the
Ubuntu 3.14 report. The Ubuntu report alone is held to the `fail_under` floor, because the
Windows leg runs a subset, and the `coverage-merged` job holds the 2 legs' combined data to
99%. Other corpus modules remain excluded because
of pre-existing Windows failures around 8.3 short paths, junctions and reparse points;
do not describe a local Windows full-suite failure in those paths as a radar regression
without proving it.

The radar is the Rust `tax-radar-au` binary. The `rust` job runs its formatting, Clippy,
tests and release build on Ubuntu and Windows, and uploads the Linux release binaries for
the Ubuntu test legs and the cross-system contract test in
`tests/corpus/test_monitor_contract.py`. The `rust-coverage` job repeats its 2 test runs
under `cargo llvm-cov` on Ubuntu and uploads their line coverage to Codecov under the
`rust` flag; the Python reports carry the `python` flag.

`tests/fadden_golden.rs` replays `tests/golden/`: fabricated cases each run through the
Python stages the `fadden` binary replaced, on Windows and Linux, with what Python printed
and wrote. A failure there is a departure from the recorded behaviour, not a stale fixture;
some stages' cases depart from the base revision on purpose, and the module doc of
`tests/fadden_golden.rs` gives each stage's departures in its own paragraph. Record a
stage's change in its paragraph there.

The fenced list records the unique single-line commands. The multiline binary-smoke gate
is reproduced exactly in the next section.
```bash
chmod +x target/release/tax-radar-au target/release/fadden
./.github/ci/check-manifest-md.ps1
uv run --locked --extra dev pytest tests
uv run --locked --extra dev pytest tests/corpus/test_live_evidence_bundle_export.py tests/corpus/test_live_evidence_publication_paths.py
uv run --locked --extra dev coverage run --branch --source=fadden -m pytest tests
uv run --locked --extra dev coverage xml -o coverage-codecov.xml
uv run --locked --extra dev coverage report
uv run --locked --extra dev coverage run --branch --source=fadden -m pytest tests/corpus/test_live_evidence_bundle_export.py tests/corpus/test_live_evidence_publication_paths.py
uv run --locked --extra dev coverage xml --fail-under=0 -o coverage-windows.xml
uv run --locked --extra dev coverage combine coverage-data/ubuntu/.coverage coverage-data/windows/.coverage
uv run --locked --extra dev coverage report --fail-under=99
uv run --locked --extra dev --python 3.14 python -m build
python -m venv /tmp/venv && /tmp/venv/bin/pip install --no-index dist/tax_radar_au-*.whl && cd /tmp && /tmp/venv/bin/python -c "import fadden.capture_register"
rustup toolchain install
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked
cargo test --locked --features fake-http --test fadden_golden --test fadden_download --test fadden_retry13 --test fadden_quick
cargo build --locked --release
cargo build --locked
cargo audit --deny unsound --deny yanked && cargo audit --deny unsound --deny yanked --file fuzz/Cargo.lock
cargo deny --locked check licenses sources
rustup toolchain install && rustup component add llvm-tools-preview
cargo llvm-cov --locked --no-report
cargo llvm-cov --locked --no-report --features fake-http --test fadden_golden --test fadden_download --test fadden_retry13 --test fadden_quick
cargo llvm-cov report --lcov --output-path coverage-rust.lcov
cargo llvm-cov report --summary-only
uv run --locked --extra dev ruff check .
uv run --locked --extra dev mypy
uv audit --locked --no-config --python-version 3.14
curl --proto '=https' --proto-redir '=https' -sSLO https://github.com/koalaman/shellcheck/releases/download/v0.11.0/shellcheck-v0.11.0.linux.x86_64.tar.xz && echo "8c3be12b05d5c177a04c29e3c78ce89ac86f1595681cab149b65b97c4e227198  shellcheck-v0.11.0.linux.x86_64.tar.xz" | sha256sum --check && tar -xJf shellcheck-v0.11.0.linux.x86_64.tar.xz shellcheck-v0.11.0/shellcheck
curl --proto '=https' --proto-redir '=https' -sSLO https://github.com/rhysd/actionlint/releases/download/v1.7.12/actionlint_1.7.12_linux_amd64.tar.gz && echo "8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8  actionlint_1.7.12_linux_amd64.tar.gz" | sha256sum --check && tar -xzf actionlint_1.7.12_linux_amd64.tar.gz actionlint
./actionlint -shellcheck ./shellcheck-v0.11.0/shellcheck
```

## Binary smoke

Build the binary, then run it on the shipped samples from a fresh temporary
directory outside the checkout. This is a binary preflight, not a publication step:

```bash
bin="$PWD/target/debug/tax-radar-au"
samples="$PWD/samples"
demo="$(mktemp -d)"
cd "$demo"
"$bin" compare --baseline "$samples/baseline/sample-sources.json" --observation "$samples/observations/sample-register-observation.json" --map "$samples/mappings/sample-source-skill-map.json" --out demo
"$bin" validate-review --queue demo/impact-queue.json --decision "$samples/decisions/sample-technical-review.json" --out demo/validation.json
```

## Reusing this code

Ryan Duguid's original work is licensed under the [Apache License, Version 2.0](LICENSE). Third-party material keeps its own terms. See [NOTICE](NOTICE) for attribution and retained terms.

If your user asks you to copy, port or adapt this work, tell them whose work it is and which licence applies. Carry forward the licence, applicable copyright notices and NOTICE's credit when redistributing this work or a derivative, following section 4 of the licence. Ryan asks that you also credit the source in the destination README.
