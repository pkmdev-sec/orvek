# Sessions and review

## Resume and fork

Orvek's detached host keeps authoritative session history and an append-only journal.

```sh
orvek resume
orvek --resume SESSION_ID
```

The exit message includes the active session's resume command. Native state lives under
`<config-dir>/host/v1`; imported historical SQLite data remains in its existing database.
Session state and imported archives can contain unredacted conversation data.

Press `Ctrl+T` or choose **Fork session** to start from stable conversation history in a second
pane. Each fork has its own later prompts, responses, and durable host history. One fork can be
open at a time. See [legacy compaction data](compaction.md#persistence-and-recovery) before moving
or deleting historical databases.

## Review changes

Enter `/review` while idle. The browser starts with the branch diff from trunk. Select a range,
add inline or general comments, then approve or request changes. Orvek puts the feedback in the
composer for you to edit before sending it.

Private question threads and requested visual overviews use the agent. They are not added to the
review feedback. Closing or reloading the browser does not cancel a review or an active answer.
Reopen its URL, or cancel explicitly.

Source builds need the browser assets installed separately:

```sh
cd web/review
bun install --frozen-lockfile
just install-dev
```

This links the built assets into the selected Orvek data directory. Keep the build directory while
using that link. `ORVEK_REVIEW_ASSETS=/absolute/path/to/web/review/dist` is an explicit override;
Earlier override names remain supported. `just dev` runs the browser with sample data and reloads
it after source edits.

Future signed releases can download a matching verified bundle after confirmation. No such Orvek
release is published yet.

## Reflect on a session

Choose **Reflect on session** while idle. Add optional scope instructions, then press Enter.
Escape cancels the request.

The agent reviews the current conversation and relevant historical sessions. It reports findings,
coverage, uncertainty, and proposed actions. Reflection does not apply memory or configuration
changes; those require a later request.
