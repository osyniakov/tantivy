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
| `columnar_reader` | `ColumnarReader::open` for the columnar (fast field) format, then opening every column handle it lists, which is where the per-column byte ranges reach the column decoders. |
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

## Seed corpus

`columnar_reader` and `sstable_dictionary` take a container format: a footer, a
declared length, offsets. Blind mutation does not synthesise one — measured on
`columnar_reader`, 300 seconds of libFuzzer from an empty corpus is 42.8M
executions for 109 edges, all of them in the length checks at the top of
`ColumnarReader::open`. The parser underneath is never entered. Seeded with a
single valid columnar, the same binary reaches the term dictionary in seconds.

So both targets ship a seed corpus, in `fuzz/seeds/<target>/`. It is small and
checked in, and `.clusterfuzzlite/build.sh` packs each directory into
`$OUT/<target>_seed_corpus.zip`, which is where libFuzzer looks for it.

Locally, pass the seeds as a second corpus directory. libFuzzer writes what it
discovers into the *first* one, so this keeps the checked-in seeds read-only
and the new units in the gitignored corpus:

```bash
cargo fuzz run columnar_reader fuzz/corpus/columnar_reader fuzz/seeds/columnar_reader
```

The seeds are generated rather than hand-written, so they stay valid as the
formats change:

```bash
cargo run --manifest-path fuzz/seeds/generator/Cargo.toml -- fuzz/seeds
```

That crate declares its own `[workspace]` and is not a fuzz target, so
`cargo fuzz build` never builds it and CI never pays for it. Regenerate the
seeds when a format version changes; a stale seed is a weaker starting point,
not a failure.

## Findings so far

Fixed, each with a regression test:

- a fieldless `Exists` resolved with `expect` — a two-character query panicked
  `parse_query`;
- a `FileSlice::split_from_end` / `slice_from_end` underflow, plus the footer
  length checks the sstable and columnar readers needed on top of it;
- a `VInt` / `VIntU128::deserialize` shift overflow on an over-long encoding;
- an unbounded loop in `set_infallible`, reachable from any query containing
  `IN[` — a 12-byte query reached 4 GB;
- a truncated sstable block header;
- the sstable value layer trusting its own block: `ValueReader::value` indexed
  past the values a block declared, `deserialize_vint_u64` decoded a truncated
  block as zeros and could overflow on an over-long encoding, and
  `VecU32ValueReader` reserved a `Vec` sized by a length it had not checked;
- the sstable key layer trusting its own block: a corrupt suffix length
  advanced past the buffer, and a corrupt prefix length made the reader
  resize its key to several exabytes;
- block addresses from the index used to slice the sstable unchecked — a
  corrupt index tripped `FileSlice`'s out-of-range assert on the first lookup;
- the v3 index trusting its footer: an fst length and a block-store length
  past the data, and a footer shorter than 8 bytes;
- the columnar column dictionary trusting its own entries: `iter_columns`
  unwrapped the column type code, indexed the key at `len - 2` and sliced the
  column data with the recorded range, so a corrupt entry panicked. Entries are
  now checked, and `iter_columns` yields an `io::Result` per column;
- `Streamer::advance` unwrapping the delta reader's error: the block checks
  above turned a corrupt sstable block into a panic one layer up. The stream
  now ends and parks the error for `take_error`, so `list_columns` reports the
  corruption instead of returning a short list;
- the columnar column decoders splitting their bytes at a length read from the
  file: the column index length in `open_column_u64` / `open_column_u128` /
  `open_column_bytes`, the optional and multivalued index lengths, and the
  blockwise-linear and compact-space footer lengths;
- `BitUnpacker::new` asserting on a bit width read from a file — the unpacker
  cannot represent [57..63], which the writer never emits but a corrupt column
  does. `BitUnpacker::new_checked` reports it instead, and the linear,
  blockwise-linear and compact-space readers use it;
- `ColumnStats::deserialize` multiplying the stored amplitude by the stored gcd
  and adding the stored minimum, both unchecked;
- `CompactSpace::deserialize` walking its ranges with unchecked additions: the
  u128 value the deltas advance, the u32 range length it narrows to, and the
  compact-space cursor could each overflow. They are checked once on
  deserialize, so `range_length` and `compact_end`, which run per lookup, stay
  as they were.

Reading a columnar is now covered by a property test rather than only by the
reproducers: `test_no_single_bit_flip_panics_while_reading_columns` flips every
bit of a columnar covering each column shape in turn, and lists and opens every
column of each. The first four findings above fail it; the last two are deeper
than a single flipped bit reaches and were found by the target itself, which is
the argument for the seed corpus.

With all of them fixed, `columnar_reader` runs its seeds for 300 seconds
(7.6M executions) without a crash, at 1222 edges against the 109 it reached
blind.

### Still open

Both reproduce on the current tree and are recorded here so the next person
does not rediscover them and assume CI is simply broken. Replay one with:

```bash
printf '%s' <hex> | xxd -r -p > /tmp/crash
cargo fuzz run sstable_dictionary /tmp/crash
```

| Where | What | Reproducer (hex) |
| --- | --- | --- |
| `tantivy-fst` (`raw/node.rs:305`), reached from `SSTableIndexV3::locate_with_key` | `Fst::new` accepts the bytes, but traversing the corrupt automaton panics. This is in the dependency, not in tantivy: the fix is for `tantivy-fst` to validate what `Fst::new` accepts (or for the v3 index to run `verify()` on open). | `0e00260001010000080000000000000000000000000000fffffffffbff002401000000000000000000000072727201027240ffffff2a07000000000000081f0a0240070000000030000000000000001f00000000000000081f0701f5f57af572727201027240ffffff2a07000000000000081f0a0240070000000030000000000000001f0000000000000010ffffffffff1e0a03000000` (151B) |
| `BlockAddrStore` in `sstable/src/index/v3.rs` | Latent, behind the fst finding: the bit-packed block-address decoder trusts its metadata (`1 << (nbits - 1)` with `nbits == 0`, `assert!(num_bits <= 56)` on file bytes, unchecked `- range_shift`, and `.unwrap()`s that hold only for self-consistent files). This is a hot path written to be unchecked on purpose, so choosing between validating on open and checking per access is a maintainer decision. | none yet — the fst crash is hit first |

### Follow-ups not yet started

- The footer pattern fixed in the sstable and columnar readers (a length read
  from the file used in `split_from_end` / `slice_from_end` unchecked) also
  appears in the main crate: `src/termdict/mod.rs`,
  `src/termdict/fst_termdict/termdict.rs` (`footer_size` is read from the
  file), `src/store/footer.rs`, `src/directory/footer.rs`. None of the current
  targets reaches them; a `Directory`-level index-open target would.
- `Streamer::take_error` has one caller so far, the columnar column dictionary.
  The term dictionary streams in the main crate (`src/termdict/`, the
  aggregation and automaton weights) still treat a corrupt block as the end of
  the stream, which no longer panics but does read a corrupt segment as a
  shorter one. Threading the error out means making `advance` fallible or
  checking `take_error` at each `while stream.advance()`, a call-site-by-call-site
  decision rather than a mechanical change.
- `cflite_batch.yml` has no `storage-repo`, so each nightly run restarts from
  the checked-in seeds and discards whatever it discovers. Configuring one lets
  the corpus accumulate between runs.
- Scorecard's own remediation is OSS-Fuzz onboarding (a `project.yaml` in
  `google/oss-fuzz`); `.clusterfuzzlite/build.sh` is reusable there as is.
- Candidate new targets: `Directory`-level index open, the doc store, and JSON
  document parsing.

## Corpus and artifacts

`fuzz/corpus/` and `fuzz/artifacts/` are gitignored: they hold the corpus a run
manages for itself and the crashes it finds, neither of which belongs in the
repository. If you find a crash, add the minimized reproducer to the relevant
crate's unit tests as a regression test rather than committing it here.

## Adding a target

1. Write `fuzz/fuzz_targets/<name>.rs`, following an existing target.
2. Add a matching `[[bin]]` entry to `fuzz/Cargo.toml` (cargo-fuzz will not see
   the target without it).
3. If the target parses a container format, add seeds: teach
   `fuzz/seeds/generator` to emit them into `fuzz/seeds/<name>/`. `build.sh`
   picks the directory up by name.
4. Check it builds and runs: `cargo fuzz run <name> -- -max_total_time=30`.

`.clusterfuzzlite/build.sh` discovers targets by globbing `fuzz/fuzz_targets/`,
so CI picks up a new one with no further changes.

## Scope

Prefer targets whose input is genuinely attacker-controlled — query strings,
document text, and serialized bytes — and whose API contract is to return an
error rather than panic. Fuzzing a function with documented preconditions (for
example the low-level `read_u32_vint`, which assumes a long enough buffer)
reports contract violations as crashes and drowns out real findings.
