# ADR-0003: tree-sitter-yaml as the CST for lossless span-splice editing

- **Status:** accepted
- **Date:** 2026-08-02
- **Milestone:** M0 spike (d)
- **Tested with:** `tree-sitter` 0.26.11, `tree-sitter-yaml` 0.7.2, `saphyr` 0.0.11 (rustc 1.96)
- **Method:** throwaway Rust harness: parse-fidelity pass over an adversarial corpus, then 13 targeted `set_scalar`/`insert_map_key` splices, each verified four ways — re-parse introduces no ERROR/MISSING nodes, byte prefix/suffix outside the splice identical, edited document still loads in a second independent parser (saphyr), and a structural delta walk proves the semantic change is _exactly_ the intended one. Corpus ships as [`fixtures/compose-corpus/`](../../fixtures/compose-corpus/); harness code does not ship.

## Question

Can tree-sitter-yaml provide byte-span-accurate CSTs over real-world-nasty YAML, reliable enough to build `mast-yaml-edit`'s closed edit vocabulary on string splices into the original buffer? (Risk #2 in the plan: if fidelity were poor, the editing pillar needed a redesign.)

## Results

**Parse fidelity: 12/12 corpus files, zero ERROR nodes, zero MISSING nodes**, and saphyr loads all 12. The corpus deliberately includes: anchors + aliases + `<<:` merge arrays with post-merge overrides; block scalars with `|+` keep-chomping (significant trailing blank line), `|2` explicit indentation indicator, and folded `>-`/`>`; comments in every position (document header/footer, trailing on keys, values, and sequence items, between pairs, inside sequences); nested flow collections; single/double-quote escapes (`''`, `\"`, `\n`, `\u`-style), UTF-8 multibyte + emoji; `${VAR:-default}`/`${VAR:?err}` interpolations as plain scalars; time-like plain keys (`22:22`), quoted keys with colons/slashes, YAML-1.1-bait keys (`no`, `"on"`), an explicit `? |` block-scalar key; multi-document streams with `%YAML 1.2` directive and `...`; `!!str`/custom/`!reset` tags; nulls in all spellings; empty values/maps/seqs; CRLF line endings; a missing trailing newline; a faithful 4-space-indented stock Sail compose file.

**Targeted edits: 13/13 verified**, including the cases most likely to break a splice editor:

| Case                                                                       | Outcome                                                                                                                                                                                                                    |
| -------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| set single-quoted scalar in 4-space Sail file (env + image swap)           | byte-exact outside span                                                                                                                                                                                                    |
| insert env key at correct 4-space nesting                                  | correct indent inferred from siblings                                                                                                                                                                                      |
| set plain scalar whose line has a trailing comment                         | comment untouched (scalar node span excludes it)                                                                                                                                                                           |
| insert after a pair whose line ends in a comment                           | line-aware insertion point lands after the comment line                                                                                                                                                                    |
| set double-quoted value containing escapes                                 | quoted text splices verbatim, loads correctly                                                                                                                                                                              |
| set scalar inside a **flow** mapping                                       | span replace works identically in flow context                                                                                                                                                                             |
| replace a folded block scalar (with internal blank line) by a plain scalar | block span covers all continuation lines; following pair unharmed                                                                                                                                                          |
| set under quoted keys (`"my.service-name"`, `"com.example/slash"`)         | quote-stripping key lookup works                                                                                                                                                                                           |
| **set inside an anchored map (`&base`)**                                   | **correctly refused**: semantic delta walk detects the alias/merge ripple into `services.*` — the plan's "targeted semantic verify" step demonstrably catches the exact failure mode whole-document equality can't express |
| insert into CRLF file                                                      | newline style detected from buffer; inserted line is CRLF                                                                                                                                                                  |
| set + insert in file with no trailing newline                              | EOF handled; newline added before appended pair                                                                                                                                                                            |

## Design facts learned (bind on `mast-yaml-edit`)

1. **Splice the concrete scalar node, not its `block_node`/`flow_node` wrapper** — wrappers can carry `anchor`/`tag` children; splicing the inner scalar preserves them. Navigation is "unwrap through `stream`/`document`/`block_node`/`flow_node`, skipping `anchor`/`tag`/`comment`/directive children".
2. **Trailing comments are siblings, not part of scalar spans** — both facts the editor relies on fell out cleanly: value replacement can't eat a comment; but _insertion_ must be line-aware (find the newline after the last pair's `end_byte`, which skips past any trailing comment) rather than splicing at node end.
3. **Pair `key`/`value` fields are reliable** across block and flow contexts (`block_mapping_pair`, `flow_pair`).
4. **Block scalar node spans include their continuation lines** (verified through the internal-blank-line case), so block→plain replacement is a plain span splice.
5. **Newline style must be detected from the buffer** and used for inserted text; with that, CRLF survives edits.
6. **Anchored-region edits require the semantic-delta gate.** The mechanical splice succeeds; only the delta walk (structural equality permitting differences at exactly the edited path) reveals the ripple. M5's vocabulary may later add anchor-aware analysis to permit intentional anchor edits with a previewed ripple; until then, refusal is correct and automatic.
7. API note: tree-sitter 0.26 uses `u32` child indices; saphyr 0.0.11's `Yaml<'input>` is Cow-based with `Value(Scalar)`/`Mapping`/`Sequence` variants and derives full `Eq` — suitable as the independent re-parse verifier in the write transaction.

## Decision

`mast-yaml-edit` proceeds as planned: tree-sitter-yaml CST + byte-span splices into the original buffer, with the write-transaction gates (generic re-parse via saphyr, targeted semantic delta, `docker compose config --quiet`) as designed. No architecture change needed. Risk #2 is retired to "engineering care": the M5 implementation inherits the corpus (now `fixtures/compose-corpus/`) as golden tests plus the property test (random edit sequences must keep `docker compose config` green).

## Open edges deferred to M5 (known, not blocking)

- Insertion into **flow** mappings/sequences (needs `, key: value` placement rules) — vocabulary decision, not a fidelity risk.
- Full double-quoted key unescaping for path lookup (spike used quote-stripping only).
- Scalar _encoding_ policy when the new value needs quoting the old value didn't (writer-side, independent of CST fidelity).
- Duplicate keys in the same mapping (compose rejects them anyway; editor should refuse with provenance).
