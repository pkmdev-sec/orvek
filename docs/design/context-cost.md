# Context cost boundaries

Orvek separates billing facts, prompt-cache behavior, and representation estimates. These values answer different questions and must not be merged into one savings number.

## Ordinary request routing

An ordinary submission first decides whether the user requests information or action. The routing request depends only on the current user input:

```text
Submission artifact
  -> input::load
  -> ordinary_classification_request(model, session, latest_input)
  -> provider
```

The request keeps current text, replaces media and review artifacts with presence markers, exposes no tools, and allows at most 64 output tokens. The full context view remains available to the answer or task after routing. This prevents unrelated history from being replayed for classification.

## Three accounting domains

### Billed totals

A provider receipt is the authority for billed cost. The durable `ProviderCost` event stores the receipt with the model-call identity. If the provider omits a receipt, Orvek can show a catalog estimate for Sol, Terra, or Luna, but it marks the total as uncertain. A provider receipt always overrides a catalog estimate. Conflicting receipts remain unknown.

Root and child model calls are separate billed work. Evaluation totals must include both.

### Prompt-cache effect

`PromptCacheIdentity` covers routing, model settings, instructions, tools, and stable segment digests. Provider usage reports input and cached-input tokens. Cached tokens still occupy the context window. A cache hit can lower provider billing without changing the context representation.

### Representation effect

Each version-3 model-call report records:

- active model and context-view revision;
- source-history and control digests;
- native or bitmap choice for each stable segment;
- an exact native/bitmap input-token count pair when the provider count endpoint is available;
- exact represented source bytes and bitmap page count;
- input, cached-input, output, and reasoning tokens;
- provider-accounted cost.

Before changing one eligible segment from native text to bitmap pages, Orvek submits both complete inputs to the active model's Responses input-token count endpoint. These requests generate no model output. Orvek changes only one segment per pair, chooses bitmap only when its count is lower, and stores the pair with the dispatched call. If either count fails, Orvek sends native text.

`RepresentationProfile` also retains support for provider-receipt comparisons. It compares a native call and a bitmap call only when source history, controls, all other segment choices, output tokens, and reasoning tokens match. The output checks prevent a shorter answer or different reasoning effort from being credited as context savings. Conflicting comparable pairs produce no estimate.

A measured pair can estimate the next carry cost for the same model and source segment. This estimate is not a billed total. The TUI labels paired and next-call savings separately from session cost and displays `unknown` when the required receipts do not exist.

## Selection

The selector uses the active model profile. Its states are:

- `Native`: no valid lower-token bitmap evidence exists.
- `MeasureBitmap`: native evidence exists, but no exact count pair exists. The host counts one rendered alternative without generating output.
- `Bitmap`: the active model counted fewer input tokens for the bitmap, or a legacy comparable receipt pair shows a lower total cost.

A tie, a failed count, or missing, invalid, or conflicting observations select native text. Selection does not change task admission, configured context windows, tool availability, generated model-call counts, or termination.

## Derived-state boundary

The append-only session journal owns history. `ContextView`, page artifacts, provider reports, and representation profiles are derived state. On migration or corruption, the host rebuilds or discards derived state and keeps exact history.

The TUI consumes durable usage and projection events. It does not render pages or choose a representation.

See [Host-owned context views](../compaction.md) for persistence and retrieval and [SnapCompact evaluation](../../evals/snapcompact/README.md) for paired measurements.
