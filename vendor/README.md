# Nanocodex context extension

These crates start from the published Nanocodex 0.5.0 archives, upstream commit
`e4eea49fc6fab06a98ff01ec8c3da8d9a729eee1`. Cargo patches both packages together so
Orvek and the tool runtime use the same Responses types.

| Package | Published archive SHA-256 |
| --- | --- |
| nanocodex-agent | 61bf178f13a34a0e77a4039644d48e823b613c7ebbcfcc6fbf88f7442cb924f8 |
| nanocodex-oai-api | feb1a99937e9af21fb452d6ebfde1151d2cfaa763c98e22540593a5e22525478 |

The context-compaction extension is maintained here until a compatible upstream
release exists. Original package manifests, source, tests and provenance are retained.
Do not modify the Cargo registry cache to develop this extension.

The agent's MCP integration test also retains `tests/fixtures/mcp-stdio-server.mjs`
from the published `nanocodex-tools` 0.5.0 package at the same upstream commit.
Its test uses this local fixture instead of requiring the upstream sibling checkout,
and both model transports point to its loopback server.
