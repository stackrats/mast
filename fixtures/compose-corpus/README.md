# compose-corpus

Adversarial YAML fixtures for edit-fidelity testing (`mast-yaml-edit`, M5 golden
tests). Originated as the M0 spike (d) corpus — see
[ADR-0003](../../docs/adr/0003-tree-sitter-yaml-edit-fidelity.md) for what each
file exercises and the measured results.

Byte-level traps are intentional and must be preserved exactly:

- `crlf.yaml` — CRLF line terminators throughout (see `.gitattributes`)
- `no-trailing-newline.yaml` — file ends without `\n`
- `block-scalars.yaml` — `|+` keep-chomping with a significant trailing blank line
- `quoting-escapes.yaml` — UTF-8 multibyte content; escapes must survive byte-splices

Editors and formatters must not touch these files; add new traps as new files
rather than "fixing" existing ones.
