# Sessions and review

## Resume and fork

Orvek's detached host keeps authoritative session history and an append-only journal.

```sh
orvek resume
orvek --resume SESSION_ID
```

The exit message includes the active session's resume command. Native state lives under
`<config-dir>/host/v1`.

Press `Ctrl+T` or choose **Fork session** to start from stable conversation history in a second
pane. Each fork has its own later prompts, responses, and durable host history. One fork can be
open at a time.

## Queue and steer follow-ups

While a turn is active, submit another prompt to add it to the detached host's durable queue.
The host owns admission and queue order. The TUI only requests changes; reconnecting does not make
the client a second queue owner.

With an empty composer, press Enter to focus the latest queued item. You can also press Tab or
Shift+Tab, or click the queue. Use the queue controls to change accepted follow-ups:

- Up and Down select an item. Shift+Up and Shift+Down reorder it.
- `e` edits the selected item. Enter saves the edit, and Escape cancels it. Saving an empty edit
  removes the item.
- Enter steers the selected item into the current task instead of waiting for its queue position.
  Press Enter twice after typing a prompt to submit and steer that prompt directly.
- `d`, Delete, or Backspace removes the selected item.

Accepted edits, removals, steering, and moves are atomic host records. Steering cancels the current
execution so the task can admit the promoted follow-up. Only queued, unclaimed items can change.
Each edit or move includes the selected input's expected digest, so the host rejects a stale change
if another client changed or claimed that item. A follow-up that has entered the accepted task
contract cannot be edited; send a new follow-up instead. The TUI displays at most 512 pending
submissions. It reports a display or consistency error instead of presenting an oversized roster or
an order that kept changing during three paging attempts.

## Hand off to a fresh context

Choose **Prepare handoff** while idle. The host requires a settled workspace checkpoint and no
unresolved shell operation. Orvek first runs a dedicated auxiliary model request over the current
conversation. It creates the new context only after that request produces a continuation draft and
the source task and workspace still match the starting checkpoint.

The result is a durable child session with the parent's settled workspace and admission settings,
but empty provider history, no current task, and no inherited outcome. The generated continuation
prompt opens as an editable draft in the new session; Orvek does not submit it automatically. The
original task, spend, and history remain unchanged.

If context creation fails after the report is available, Orvek restores the draft in the source
session. Creation retries an uncertain acknowledgement with the same new session ID, up to three
attempts, without rerunning the report. If acknowledgement remains uncertain, the error names the
session ID that may have been created so you can resume it. Cancellation after report generation
also retains the draft.

## Resume imported historical sessions

`orvek resume` lists matching entries from `<config-dir>/sessions/v2.sqlite3` as **Historical** when
that database exists. Selecting one takes a bounded, read-only snapshot of its selected lineage and
imports it into a new authoritative host session under `<config-dir>/host/v1`. Later prompts and
host records belong to that imported session. Orvek does not update or delete the source database.

The picker requires the historical catalog to end before row 500. At 500 or more rows, it reports
a display-limit error instead of showing a truncated list. Import rejects corrupt or
unsupported records and settings rather than selecting replacement model settings. Default bounds
include a 64 MiB database snapshot, 100,000 selected records, 1 MiB per record, 32 MiB of selected
data, 64 lineage levels, and a 30-second import operation. A retry of the same import request reuses
its committed result; content-identical later imports can reuse published progress.

Session state, source databases, and imported archives can contain unredacted conversation data.
Unknown historical tables remain opaque data in the retained snapshot and never grant tool or task
authority. Keep the source database if vendor-private archives matter. See
[legacy compaction data](compaction.md#persistence-and-recovery) before moving or deleting it.

## Submit images

Copy an image, then press Ctrl+V or Cmd+V in the TUI. Orvek converts clipboard pixels to PNG and
inserts an image marker into the editable draft. When the host accepts the prompt, it stores the
image bytes as content-addressed input artifacts. The attachment survives queue edits, session
replay, and resume; it is user media, not tool or policy authority.

Input admission accepts PNG, JPEG, GIF, and WebP only when the declared media type matches the file
signature. One prompt can contain at most eight images and 64 ordered content parts. Decoded image
bytes must total at most 4 MiB, and each embedded data URL must be at most 12 MiB. Orvek rejects an
over-limit or invalid prompt before provider dispatch.

At dispatch, Orvek converts accepted artifacts to provider `input_image` parts. The configured
provider and model must accept image input and may impose stricter size, count, or token limits;
Orvek's admission limits do not establish provider support. Terminal image support affects display
only. A text marker remains available when the terminal cannot render an image.

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
