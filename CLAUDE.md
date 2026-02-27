# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

`axum-markdown` is a Rust library crate providing Tower middleware for Cloudflare-style "Markdown for Agents" content negotiation. When a client sends `Accept: text/markdown`, the middleware intercepts HTML responses, converts them to markdown, counts tokens (optionally), and sets appropriate headers (`Content-Type: text/markdown`, `Content-Length`, `x-markdown-tokens`, `content-signal`, `Vary: Accept`). Non-HTML responses and requests without `Accept: text/markdown` pass through unchanged (with `Vary: Accept` appended).

## Build & Test Commands

This project uses Nix flakes with crane for builds. Inside the dev shell (`nix develop`):

- **Build:** `cargo build`
- **Test:** `cargo test`
- **Test (no default features):** `cargo test --no-default-features`
- **Single test:** `cargo test test_name`
- **Clippy:** `cargo clippy --all-targets -- -D warnings`
- **Clippy (no default features):** `cargo clippy --no-default-features --all-targets -- -D warnings`
- **Format:** `cargo fmt` / `cargo fmt --check`
- **Run example:** `cargo run --example basic`
- **Nix checks (build + fmt + clippy + tests + no-default checks):** `nix flake check`

Toolchain: stable Rust with rustfmt, clippy, rust-src, rust-analyzer (managed via `rust-toolchain.toml`).

## Feature Flags

- `tokens` (default) — enables `tiktoken-rs` dependency and `TiktokenCounter`. When off, no `x-markdown-tokens` header is emitted.
- `tracing` — enables `tracing` spans/events in the conversion path.

## Architecture

Single-file library (`src/lib.rs`) with these public types:

- **`HtmlConverter`** trait — converts HTML to markdown. Default impl: `HtmdConverter` (wraps `htmd::convert`).
- **`TokenCounter`** trait — counts tokens. Default impl: `TiktokenCounter` (wraps `tiktoken-rs`, gated behind `tokens` feature).
- **`MarkdownConfig`** — Builder-style config (`#[non_exhaustive]`, private fields). Configures max body size, content-signal, converter, token counter, and skip predicate.
- **`MarkdownLayer`** — `tower::Layer` implementation; wraps services with `MarkdownService`. Implements `From<MarkdownConfig>`.
- **`MarkdownService<S>`** — `tower::Service` implementation; checks skip predicate, then `Accept` header, delegates to inner service, then conditionally converts HTML responses to markdown.
- **`WantsMarkdown`** — Infallible `FromRequestParts` extractor; returns `true` if `Accept: text/markdown` is present.

The future uses a two-phase state machine (`FutureState::Pending` → `FutureState::Converting`) via `pin_project_lite` to handle the async body read during conversion without boxing the common passthrough path.

Key internal functions: `wants_markdown()` (Accept header parsing), `is_html_response()` (Content-Type check), `convert_response()` (body read + converter trait dispatch + optional token counting + header rewriting), `append_vary()`.

All tests are inline in `src/lib.rs` and use `tower::ServiceExt::oneshot` with an axum `Router`. Token-count assertions are gated behind `#[cfg(feature = "tokens")]`.
