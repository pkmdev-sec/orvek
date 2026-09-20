# Frozen workspace review

`inspect` captures a Git range into immutable artifacts. `catalog` returns at most
32 related local/remote branches and 64 first-parent commits for a picker, with
explicit truncation flags. Pass returned full object IDs to pin selections.

Ranges are working tree against an optional base (HEAD by default), staged index
against an optional base, one commit against its first parent, or two commits.
The root commit compares with an empty tree. Working-tree capture includes
tracked files and untracked files allowed by repository ignore rules. It retains
symlink targets as bytes and never follows them for file retrieval. Gitlinks are
opaque commit references in staged/historical views; working submodules must be
reviewed separately. Empty directories are not part of a Git diff.

The returned `ReviewInspection` contains a manifest digest, source identity,
patch digest, two frozen-tree manifest digests, and resolved base/head IDs.
`file` and `page` resolve only through that manifest and the selected side.
`FrozenFile.content` is an artifact digest; host `ReadArtifact` may serve its
bytes. These trees are not `workspace::Snapshot` values. Files never fall back
to the current working tree. A repeated unchanged capture has the same identity.
No result is task completion, evaluator evidence, or permission to mutate.

The Git transport uses a fixed system executable, cleared environment, empty
HOME/config/template/hooks directories, and fresh Git metadata. Original Git
metadata is used for bounded name/index/object-location discovery. Object
traversal, diffs, and index operations run in fresh metadata with no remote or
repository configuration. Objects are read through an alternate object directory
and verified by their SHA-1/SHA-256 blob identity. Source filters, external diff,
textconv, hooks, fsmonitor, replacement objects, and network helpers are not used.
The host never runs repository executables. A deadline, process group cleanup,
stdin limits, and captured-output limits bound each utility operation.

Working files are opened relative to directory descriptors with `NOFOLLOW`.
Capture compares multiple passes plus HEAD and index identities; observed source
changes fail. This is a checked capture, not a filesystem-wide atomic snapshot or
a promise that the source will remain unchanged after return. Publication is of
the frozen artifact identities only. Split indexes are copied with bounded shared
index bytes; the original index and source files are never written.

Default bounds are 10,000 files, 16 MiB per file, 64 MiB per tree, 32 MiB patch,
4 MiB metadata, and 30 seconds. Larger supported bounds are explicit in
`ReviewLimits`; overflow fails without a partial successful inspection. Ref names
and numeric `^`/`~` operators are accepted; arbitrary revision expressions are
rejected. Unborn/bare repositories, non-UTF-8 paths, special files, working
submodules, unmerged indexes, and missing local objects fail explicitly. No
remote fetch is attempted to fill missing objects. Queries verify stored manifest
identities and have bounded page sizes.

Run the offline native Git tests with:

```
cargo test -p orvek-harness --test workspace_review --offline -- --test-threads=1
```

The environment-isolation fixture marked ignored is executed by its parent test
in a poisoned child environment. No test opens user session databases or auth.

`inspect_snapshots(before, after, artifacts, cancellation)` serves task review
without a source checkout. It validates and preserves the actual Snapshot
artifacts and uses fresh Git metadata with no object alternates. The range is
`Snapshots { before, after }`, binding the exact source identities. Directory
entries (including empty directories) and full file permissions remain in the
frozen trees. `metadata_changes` explicitly lists changes that a Git patch alone
cannot show, such as new empty directories or nonstandard permission changes.
The browser must show this notice alongside the full-context patch. This is a
review view, not an assertion that Git apply reproduces all metadata; only the
separate delivery builder may establish that result. `InspectWorkspace` rejects
the snapshot range, which is prepared only through the typed snapshot API.
