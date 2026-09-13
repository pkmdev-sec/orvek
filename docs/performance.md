# Performance notes

The renderer reuses wrapped draft lines for cursor movement, borrows text while building lines,
caches syntax styles, and avoids repeated layout work for pinned prompts. Unicode handling,
selection, and the thinking animation remain covered by behavior tests.

The refined TUI also caches notification wrapping, the child-agent tree layout, and queue
scrolling state. Decorative animation is scheduled only while visible work is unsettled; notices
use one expiry deadline rather than a polling timer.

The scheduler combines streaming updates and lets keyboard input request an immediate frame.
Long responses and terminal I/O still add work; a frame-rate limit is not a throughput guarantee.

## Local checks

```sh
cargo bench --locked --bench tui -- 'tui/' \
  --sample-size 20 --warm-up-time 1 --measurement-time 1 --noplot
```

The fixtures cover large drafts, cursor movement, accumulated Markdown, paging, and expanded tool
output. They measure local rendering, not model latency or task success.

Hosted CodSpeed upload is optional. Configure repository access with the service, then set the
GitHub repository variable `ORVEK_ENABLE_CODSPEED` to `true`. Local runs need no hosted account.
