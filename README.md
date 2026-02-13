# axum-markdown

Tower middleware for [Cloudflare-style "Markdown for Agents"](https://blog.cloudflare.com/markdown-for-bots/) content negotiation in [axum](https://github.com/tokio-rs/axum).

When a client sends `Accept: text/markdown`, the middleware intercepts HTML responses, converts them to markdown, counts tokens, and sets appropriate headers. Non-HTML responses and requests without `Accept: text/markdown` pass through unchanged.

## Response Headers

On conversion, the middleware sets:

| Header | Value |
|---|---|
| `Content-Type` | `text/markdown; charset=utf-8` |
| `x-markdown-tokens` | Token count (using `o200k_base` tokenizer) |
| `content-signal` | `ai-train=yes, search=yes, ai-input=yes` (configurable) |
| `Vary` | `Accept` (always set, even on passthrough) |

## Usage

Add the dependency:

```toml
[dependencies]
axum-markdown = "0.1"
```

Add the layer to your router:

```rust
use axum::{Router, response::Html, routing::get};
use axum_markdown::MarkdownLayer;

let app = Router::new()
    .route("/", get(|| async { Html("<h1>Hello</h1><p>World</p>") }))
    .layer(MarkdownLayer::new());
```

### Custom Configuration

```rust
use axum_markdown::{MarkdownConfig, MarkdownLayer};

let config = MarkdownConfig::new()
    .max_body_size(5 * 1024 * 1024)       // 5MB limit (default: 10MB)
    .content_signal("ai-train=no");        // custom Content-Signal value

let layer = MarkdownLayer::with_config(config);
```

To disable the `content-signal` header entirely:

```rust
let config = MarkdownConfig::new().no_content_signal();
```

## Example

```sh
cargo run --example basic
```

```sh
# Normal HTML response
curl http://localhost:3000/

# Markdown response
curl -H 'Accept: text/markdown' http://localhost:3000/
```

## License

MIT
