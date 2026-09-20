# Releases

Orvek currently ships from source. No Orvek crates, signed binaries, or container images are
published. The release workflow is not ready until these prerequisites are complete:

- Release or upstream the pinned Nanocodex extensions and update dependencies.
- Establish ownership of `orvek` and `orvek-memory` on crates.io.
- Set `CARGO_REGISTRY_TOKEN` with permission to publish those crates.
- Allow GitHub Actions to create releases and publish GHCR packages.

Do not bypass package verification.

## Publish a tag

Bump the workspace version, refresh `Cargo.lock`, commit, and push `main`. Wait for the `main` CI run
to pass. Pin the fetched commit before reading its version or creating a tag:

```sh
set -eu
git fetch origin main
release_commit=$(git rev-parse origin/main)
version=$(git show "$release_commit:Cargo.toml" | awk -F'"' '$1 == "version = " { print $2; exit }')
test -n "$version"
git tag "v${version}" "$release_commit"
git push origin "refs/tags/v${version}"
```

Never move or reuse a published release tag.

## Workflow outputs

The tag workflow checks the version and main ancestry, then:

1. Builds Linux x86-64/ARM64 and macOS Intel/Apple Silicon binaries.
2. Packages, checksums, and signs binary archives and the review bundle.
3. Publishes the library crates and binary crate with signing metadata.
4. Creates a GitHub Release from `.github/RELEASE_TEMPLATE.md` and `git-cliff` history.
5. Publishes GHCR images containing the verified Linux binaries.

Use Conventional Commits for changelog entries. Non-conventional messages are omitted. Release
publication waits for crates.io; container publication waits for the GitHub Release.

## Retry a failed release

Keep the original tag and reuse its saved signing bundle. Check what crates.io, GitHub, and GHCR
already accepted before rerunning jobs. If source changes are needed, use a new version and tag.
