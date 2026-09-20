# orvek-memory

Memory storage and tools for host applications. This workspace crate ships with Orvek 0.1.0
source and has no crates.io release.

| API | Purpose |
| --- | --- |
| `MemoryStore` | Async operations with version checks and storage limits. |
| `LocalMemoryStore` | Local SQLite storage using schema v1. |
| `SelectedMemoryStore` | Select one local or remote backend per runtime. |
| `RemoteMemoryClient`, `server::MemoryServer` | Authenticated HTTP operations with author namespaces. |
| `MemorySession`, `MemoryPermission` | Provider-neutral operations and host-supplied access. |
| `MemoryTool`, `MutationAuthorizer` | Agent scan, read, put, and delete with application-controlled write access. |

Default features are `client`, `local`, `native-server`, and `tool`. For a server without native
client or SQLite dependencies, disable default features and enable `server`.

Client storage checks reject content flagged as secret-like and filter flagged records before use.
Server stores enforce storage rules separately. See the [memory guide](../../docs/memory.md)
for configuration, permissions, limits, and transfer commands.
