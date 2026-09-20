# Codebase graph

This directory is the durable entry point for an agent that has no prior Orvek context. Read
[`../../CODEBASE.md`](../../CODEBASE.md), then use this directory in the order below.

1. Read [`overview.md`](overview.md) for the architecture and the paths that own each concern.
2. Query [`graph.json`](graph.json) when you need exhaustive file, directory, Cargo package, Rust
   module, dependency, or curated-component relationships.
3. Render [`architecture.dot`](architecture.dot) when a compact responsibility diagram is useful.
4. Open the source files attached to the relevant graph nodes. The graph is a navigation index, not
   an authority on runtime behavior.

## Machine-readable graph

`graph.json` is deterministic and uses schema version 1. Node IDs are stable while the represented
path or package remains stable:

| Node ID shape | Meaning |
| --- | --- |
| `file:<repository-relative-path>` | A tracked repository input file. |
| `directory:<repository-relative-path>` | A directory containing indexed inputs. |
| `package:<Cargo package name>` | A Cargo package; `workspace_member` distinguishes a workspace member from a vendored package. |
| `dependency:<crate name>` | A direct dependency external to this workspace. |
| `target:<package>:<target>` | A Rust target group derived from explicit Cargo targets or conventional source locations. |
| `module:<package>:<target>::<path>` | A Rust source module. |
| `component:<name>` | A curated cross-file architectural responsibility. |

Each edge has `source`, `kind`, and `target`. `relationship_kinds` defines every `kind`; edge
semantics are directional. `statistics.static_reference_coverage` reports references recognized by
the generator and how many resolve to tracked files. It covers Rust compile-time includes and static
relative TypeScript imports, including side-effect imports; generation fails on an unresolved match. Start at a `component:` node for a concern, follow `implemented_by` to
source, then follow `declares_module`, `imports`, `imports_local`, `includes`, or `depends_on` as needed. Start at
`repository:orvek` or `directory:.` for a complete crawl.

The graph covers version-controlled inputs and these authored map documents. It intentionally
excludes Git internals, ignored dependencies, build products, untracked working-copy material, and
its generated outputs. That prevents a fresh agent from treating local experiments or secrets as
repository architecture.

## Regeneration contract

The generator uses only Python's standard library and must be run from any working directory:

```sh
python3 scripts/generate-codebase-graph.py          # update graph.json and architecture.dot
python3 scripts/generate-codebase-graph.py --check  # fail when either derived file is stale
```

Run the update after changing tracked file layout, Cargo manifests, file-backed Rust module
statements, Rust compile-time includes, static relative TypeScript imports, local Python imports, Markdown
local links, or the curated component map in the script. The generated graph has no timestamp or absolute paths, so a clean checkout produces the
same bytes.

Extraction is deliberately conservative. File-backed Rust `mod` declarations, compile-time include macros, package-level Cargo dependencies,
static relative TypeScript imports, unambiguous local Python imports, and Markdown local links are indexed;
macro-generated or inline module topology is not inferred. The
component edges are curated architectural claims with source attachments, not substitutes for code
review or behavior checks.
