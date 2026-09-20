# Sloppiness diagnostics

Orvek exposes a host-owned `measure_sloppiness` tool to coding tasks. It computes deterministic
signals from source in the task workspace; it does not ask the model to judge its own code or call a
shell, compiler, network service, or external analyzer.

The design follows the useful quantitative ideas in [Measuring the sloppiness of
code](https://earendil.com/posts/measuring-code-sloppiness/) while preserving the article's main
warning: no single measurement is code quality, and a metric stops being informative when it
becomes the sole optimization target.

## Language handling

Report version 2 discovers source independently of a closed language list. Common languages,
configuration formats, and extensionless scripts receive conventional comment handling. A textual
file with an unfamiliar extension is still analyzed under that extension's name, so adding a new
language does not require an Orvek release. Documentation, lock files, common binary formats, and
non-text files are excluded. `language` names the sole detected language or is `mixed`; `languages`
contains deterministic per-language file counts.

All detected languages contribute to source-line and clone metrics. Redundant-AST and complexity
analysis is richer and therefore adapter-specific: the current Rust adapter supplies those signals.
Files without an adapter remain in the universal metrics and produce an explicit, aggregated
limitation instead of being omitted or treated as complexity-free.

## Report

The JSON report contains three independent signals:

- `source_lines`: non-blank, non-comment source lines. In an isolated task,
  `delta.source_lines` is the candidate value minus the frozen baseline value.
- `verbosity.ratio`: the union of duplicated lines and lines matched by conservative redundant-AST
  rules, divided by source lines. Taking the union prevents double counting. Clone detection works
  across detected languages using repeated normalized four-line windows with at least 80
  non-whitespace bytes. The Rust AST adapter identifies boolean-literal branches, boolean-only
  matches, identity `map` closures, and always-true `filter` closures.
- `erosion.ratio`: for adapted files, the mass in functions with cyclomatic complexity greater than
  10 divided by all function mass, where `mass(f) = CC(f) * sqrt(SLOC(f))`. The largest masses are
  returned as bounded hotspots.

The analyzer bounds files, bytes, and returned hotspots; ignores dependency, VCS, build, and common
generated-output roots; does not follow symlinks; and reports parser and adapter limitations.

## Harness behavior

`measure_sloppiness` is read-only and available during discovery and implementation in both runtime
modes. Its current root is the admitted session workspace—not the detached host's state directory,
socket, or process working directory.

- Isolated tasks receive current and frozen-baseline reports plus deltas.
- Native tasks receive the current report only because native execution intentionally has no
  snapshot or immutable baseline.

The report is diagnostic evidence, not protected verification. It cannot satisfy behavior checks,
override a failed check, or complete a task. A repository may use trends as one review input, but
Orvek deliberately supplies no universal pass/fail threshold or combined “quality score.”

The public `orvek_harness::sloppiness` module also exposes `analyze`, `assess`, `compare`, and a
bounded `AnalysisPolicy` for host integrations that need the same implementation outside a model
turn.
