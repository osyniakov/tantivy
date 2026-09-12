# Fuzzing tantivy

Fuzz targets for tantivy, built on [`cargo-fuzz`] and libFuzzer. Each target
feeds mutated, adversarial input into a parser or deserializer and asserts that
it rejects bad input cleanly instead of panicking, reading out of bounds, or
looping forever.

The same targets run in CI through [ClusterFuzzLite]: `.github/workflows/cflite_pr.yml`
fuzzes the code a pull request touches, and `.github/workflows/cflite_batch.yml`
runs every target for longer on a daily schedule. The container that builds them
is defined in `.clusterfuzzlite/`.

[`cargo-fuzz`]: https://rust-fuzz.github.io/book/cargo-fuzz.html
[ClusterFuzzLite]: https://google.github.io/clusterfuzzlite/

## Prerequisites

```bash
rustup toolchain install nightly   # libFuzzer needs nightly
cargo install cargo-fuzz
```

## Running

All commands are run from the repository root.

```bash
cargo fuzz list                                  # show the available targets
cargo fuzz run query_grammar                     # fuzz until you stop it
cargo fuzz run query_grammar -- -max_total_time=60   # time-box a run
cargo fuzz run -j 8 query_grammar                # parallel jobs (a cargo-fuzz flag)
```

Anything after `--` goes to libFuzzer itself; see `cargo fuzz run <target> -- -help=1`.

## Targets

| Target | What it fuzzes |
| --- | --- |
| `query_grammar` | `tantivy-query-grammar`'s `parse_query` / `parse_query_lenient` — the raw query-string grammar. |
| `query_parser` | `QueryParser` against a real schema: field resolution, typed values, ranges. |
| `tokenizer` | The simple, whitespace, raw and ngram tokenizers plus the lowercase, remove-long, ASCII-folding and compound-splitting filters. Asserts that token offsets stay in bounds and on UTF-8 character boundaries. |
| `sstable_dictionary` | `Dictionary::from_bytes` for the SSTable term dictionary format. |
| `columnar_reader` | `ColumnarReader::open` for the columnar (fast field) format. |
| `common_vint` | `VInt` / `VIntU128` decoding, including a serialize/deserialize round-trip check. |

The byte-oriented targets (`sstable_dictionary`, `columnar_reader`, `common_vint`)
exist because those formats carry their own offsets and lengths: a truncated or
hostile buffer can point anywhere, and the API contract is to return an `Err`.

## Reproducing and minimizing a crash

A crash is written to `fuzz/artifacts/<target>/`. Replay it by passing the file
back to the same target:

```bash
cargo fuzz run query_grammar fuzz/artifacts/query_grammar/crash-<hash>
cargo fuzz tmin query_grammar fuzz/artifacts/query_grammar/crash-<hash>
```

`tmin` shrinks the input to the smallest one that still reproduces, which
usually makes the root cause obvious.

Targets are built with `--debug-assertions` in CI, so integer overflow and
`debug_assert!` failures count as crashes. That is deliberate: an arithmetic
overflow that merely wraps in release is still a bug, and it is far easier to
diagnose as a panic.

## Known open findings

This reproduces on the current tree. It is recorded here so the next person does
not rediscover it and assume CI is simply broken. Reproduce it by writing the
bytes to a file and replaying them:

```bash
printf '%s' 0a00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001f1f0a02000000 \
  | xxd -r -p > /tmp/crash
cargo fuzz run sstable_dictionary /tmp/crash
```

| Target | Symptom | Reproducer (hex) |
| --- | --- | --- |
| `sstable_dictionary` | Panic indexing out of bounds in `IndexValueReader::value` (`sstable/src/value/index.rs`), reached from `SSTableIndex::open`. | `0a0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001f1f0a02000000` (68B) |

This one is not a local oversight, which is why it is not simply patched:

- All four `ValueReader::value(idx)` impls index `self.vals[idx]` unchecked, and
  the trait signature (`fn value(&self, idx: usize) -> &Self::Value`) cannot
  report an error. A block whose key section decodes more entries than its value
  section declared therefore indexes past the end.
- `deserialize_vint_u64` (`sstable/src/value/mod.rs`) does `*data = &data[num_bytes..]`
  with no check that `num_bytes` is in bounds.

The layer was written for self-produced, trusted files. Hardening it means
deciding on a contract: the natural fix is to add `fn num_values(&self) -> usize`
to `ValueReader` and have the block readers refuse an out-of-range index, but
`ValueReader` is public API, so a new required method is a breaking change.
That is a call for the maintainers rather than something to settle inside a
fuzzing change.

Everything else found so far is fixed, with regression tests: a fieldless
`Exists` `expect`, a `FileSlice::split_from_end` underflow, a `VInt::deserialize`
shift overflow, an unbounded `set_infallible` loop reachable through `IN[`, and a
truncated sstable block header.

Three earlier findings — a fieldless `Exists` `expect`, a `FileSlice::split_from_end`
underflow, and a `VInt::deserialize` shift overflow — are fixed, with regression
tests in `query-grammar`, `tantivy-common`, `tantivy-sstable` and `tantivy-columnar`.

## Corpus and artifacts

`fuzz/corpus/` and `fuzz/artifacts/` are gitignored — no seed corpus is checked
in, and each CI run starts from whatever corpus it manages itself. If you find a
crash, add the minimized reproducer to the relevant crate's unit tests as a
regression test rather than committing it here.

## Adding a target

1. Write `fuzz/fuzz_targets/<name>.rs`, following an existing target.
2. Add a matching `[[bin]]` entry to `fuzz/Cargo.toml` (cargo-fuzz will not see
   the target without it).
3. Check it builds and runs: `cargo fuzz run <name> -- -max_total_time=30`.

`.clusterfuzzlite/build.sh` discovers targets by globbing `fuzz/fuzz_targets/`,
so CI picks up a new one with no further changes.

## Scope

Prefer targets whose input is genuinely attacker-controlled — query strings,
document text, and serialized bytes — and whose API contract is to return an
error rather than panic. Fuzzing a function with documented preconditions (for
example the low-level `read_u32_vint`, which assumes a long enough buffer)
reports contract violations as crashes and drowns out real findings.
