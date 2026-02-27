# axum-markdown

Tower middleware for [Cloudflare-style "Markdown for Agents"](https://blog.cloudflare.com/markdown-for-bots/) content negotiation in [axum](https://github.com/tokio-rs/axum).

When a client sends `Accept: text/markdown`, the middleware intercepts HTML responses, converts them to markdown, counts tokens, and sets appropriate headers. Non-HTML responses and requests without `Accept: text/markdown` pass through unchanged.

## Response Headers

On conversion, the middleware sets:

| Header | Value |
|---|---|
| `Content-Type` | `text/markdown; charset=utf-8` |
| `Content-Length` | Byte length of the markdown body |
| `x-markdown-tokens` | Token count (requires `tokens` feature) |
| `content-signal` | `ai-train=yes, search=yes, ai-input=yes` (configurable) |
| `Vary` | `Accept` (always set, even on passthrough) |

## Feature Flags

| Feature | Default | Description |
|---|---|---|
| `tokens` | yes | Token counting via `tiktoken-rs` (`o200k_base`). Adds the `x-markdown-tokens` header and enables `TiktokenCounter`. |
| `tracing` | no | Emits `tracing` spans and events during conversion. |

To disable token counting (removes the `tiktoken-rs` dependency):

```toml
[dependencies]
axum-markdown = { version = "0.1", default-features = false }
```

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

let app: Router = Router::new()
    .route("/", get(|| async { Html("<h1>Hello</h1><p>World</p>") }))
    .layer(MarkdownLayer::new());
```

### Custom Configuration

```rust
use axum_markdown::{MarkdownConfig, MarkdownLayer};

let config = MarkdownConfig::new()
    .max_body_size(5 * 1024 * 1024)       // 5MB limit (default: 1MB)
    .content_signal("ai-train=no");        // custom Content-Signal value

let layer = MarkdownLayer::with_config(config);
```

To disable the `content-signal` header entirely:

```rust
# use axum_markdown::MarkdownConfig;
let config = MarkdownConfig::new().no_content_signal();
```

You can also convert a config directly into a layer:

```rust
# use axum_markdown::{MarkdownConfig, MarkdownLayer};
let layer: MarkdownLayer = MarkdownConfig::new()
    .no_content_signal()
    .into();
```

### Custom Converter and Token Counter

Implement `HtmlConverter` or `TokenCounter` to replace the defaults:

```rust
use axum_markdown::{MarkdownConfig, HtmlConverter, TokenCounter, ConvertError};

struct MyConverter;
impl HtmlConverter for MyConverter {
    fn convert(&self, html: &str) -> Result<String, ConvertError> {
        Ok(html.to_string()) // your logic here
    }
}

struct MyCounter;
impl TokenCounter for MyCounter {
    fn count_tokens(&self, text: &str) -> usize {
        text.split_whitespace().count() // word count as proxy
    }
}

let config = MarkdownConfig::new()
    .converter(MyConverter)
    .token_counter(MyCounter);
```

To disable token counting (omits the `x-markdown-tokens` header):

```rust
# use axum_markdown::MarkdownConfig;
let config = MarkdownConfig::new().no_token_counter();
```

### Skipping Routes

Use `skip_when` to bypass conversion for specific requests:

```rust
use axum_markdown::MarkdownConfig;

let config = MarkdownConfig::new()
    .skip_when(|req| req.uri().path().starts_with("/api"));
```

Skipped requests still get `Vary: Accept` but no conversion.

### `WantsMarkdown` Extractor

The `WantsMarkdown` extractor lets handlers check whether the client requested markdown:

```rust
# use axum_markdown::WantsMarkdown;
# use axum::response::IntoResponse;
async fn handler(WantsMarkdown(wants_md): WantsMarkdown) -> impl IntoResponse {
    if wants_md {
        "Client wants markdown"
    } else {
        "Client wants HTML"
    }
}
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

## Memory Usage

Each in-flight conversion buffers the full response body, the converted markdown string, and a token encoding vector in memory simultaneously. With the default 1MB `max_body_size`, worst-case memory per concurrent conversion request is roughly 4MB.

For production deployments, consider placing a concurrency limit in front of this middleware to bound total memory usage:

```rust
# use axum::{Router, routing::get};
# use axum_markdown::MarkdownLayer;
use tower::limit::ConcurrencyLimitLayer;
# async fn handler() -> &'static str { "" }

let app: Router = Router::new()
    .route("/", get(handler))
    .layer(MarkdownLayer::new())
    .layer(ConcurrencyLimitLayer::new(64));  // at most 64 concurrent conversions
```

## License

MIT
