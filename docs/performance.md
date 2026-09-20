# Performance notes

The renderer reuses wrapped draft lines for cursor movement, borrows text while building lines,
caches syntax styles, and avoids repeated layout work for pinned prompts. Unicode handling,
selection, and the thinking animation remain covered by behavior tests.

The refined TUI also caches notification wrapping, the child-agent tree layout, and queue
scrolling state. Decorative animation is scheduled only while visible work is unsettled; notices
use one expiry deadline rather than a polling timer.

The scheduler combines streaming updates and lets keyboard input request an immediate frame.
Long responses and terminal I/O still add work; a frame-rate limit is not a throughput guarantee.

## Provider prefix cache reuse

Root sessions route provider prompt caching with their session ID. A fork keeps its own session and
thread IDs but inherits the root's cache-routing key, allowing the provider to reuse an exact shared
prefix instead of recomputing it. Diverged prefixes remain separate because the provider still
matches request content; a miss simply processes Orvek's complete projected request.

This applies the deployment principle from [DeepSeek-V4.1-Flash: Pushing the Limits of KV Cache
Compression](https://paperswithcode.co/pdf/114097): separate reusable global cache identity from
short-lived local session state, and make cache misses a safe recomputation path. Orvek does not
implement the paper's model architecture, sparse attention, or FP4 cache format.

## Local checks

```sh
cargo bench --locked --bench tui -- 'tui/' \
  --sample-size 20 --warm-up-time 1 --measurement-time 1 --noplot
```

The fixtures cover large drafts, cursor movement, accumulated Markdown, paging, and expanded tool
output. They measure local rendering, not model latency or task success.

Hosted CodSpeed upload is optional. Configure repository access with the service, then set the
GitHub repository variable `ORVEK_ENABLE_CODSPEED` to `true`. Local runs need no hosted account.
