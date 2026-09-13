# orvek-subagents

Child-agent sessions, task trees, messages, and validated results for Nanocodex applications. This
workspace crate ships with Orvek 0.1.0 source and has no crates.io release.

Create one `Subagents` runtime per root configuration. Provide a factory for clean child sessions
and continuously drain its `ScopedAgentUpdate` receiver. Capture the weak handle from
`runtime.downgrade()` in the session tool factory and call `WeakSubagents::install_tools`. This
avoids an ownership cycle, and the inherited factory allows children to delegate further.

`RootAgentAuthority` restricts application tools to registered root sessions. The runtime also
checks tree scope, management authority, turn capacity, and result schemas.

Child state is process-local. The crate does not isolate filesystem access, persist live children,
or provide distributed jobs. Root and child sessions use the embedding application's process and
tool permissions. See [subagents](../../docs/subagents.md) for configuration and tool contracts.
