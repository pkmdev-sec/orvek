# Context cost boundaries

## Ordinary request classification

An ordinary submission first decides whether the user requests information or
action. That routing call must depend only on the current user input:

```text
Submission artifact
  -> input::load
  -> ordinary_classification_request(model, session, latest_input)
  -> provider
```

`ordinary_classification_request` deliberately has no session-history
parameter. It preserves current text, replaces image and review artifact
references with small presence markers, exposes no tools, and allows at most 64
output tokens. The full projected conversation remains available to the answer
or task run after routing.

This boundary prevents a routing decision from replaying as much as 90% of the
configured context window on every ordinary submission. With the supported
1,000,000-token window, the previous design could project approximately 900,000
tokens into classification. The new design is bounded by the current text input
(128 KiB at admission) plus small attachment markers.

Alternatives considered:

- A smaller fixed history suffix still pays repeatedly for unrelated context
  and makes the classifier's dependency on prior turns implicit.
- A cheaper model changes model-selection policy and routing quality without
  removing the redundant input.
- Local keyword routing removes provider cost but is too brittle for natural
  language intent.

The regression test inspects the provider request and verifies that only the
current text and attachment markers are present, with neither attachment
digests nor payloads.
