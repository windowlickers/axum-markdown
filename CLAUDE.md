# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

`axum-markdown` is a Rust library crate providing Tower middleware for Cloudflare-style "Markdown for Agents" content negotiation. When a client sends `Accept: text/markdown`, the middleware intercepts HTML responses, converts them to markdown (via `htmd`), counts tokens (via `tiktoken-rs` with o200k_base), and sets appropriate headers (`Content-Type: text/markdown`, `x-markdown-tokens`, `content-signal`, `Vary: Accept`). Non-HTML responses and requests without `Accept: text/markdown` pass through unchanged (with `Vary: Accept` appended).

## Build & Test Commands

This project uses Nix flakes with crane for builds. Inside the dev shell (`nix develop`):

- **Build:** `cargo build`
- **Test:** `cargo test`
- **Single test:** `cargo test test_name`
- **Clippy:** `cargo clippy --all-targets -- -D warnings`
- **Format:** `cargo fmt` / `cargo fmt --check`
- **Run example:** `cargo run --example basic`
- **Nix checks (build + fmt + clippy + tests):** `nix flake check`

Toolchain: stable Rust with rustfmt, clippy, rust-src, rust-analyzer (managed via `rust-toolchain.toml`).

## Architecture

Single-file library (`src/lib.rs`) with three public types:

- **`MarkdownConfig`** — Builder-style config (max body size, content-signal header value)
- **`MarkdownLayer`** — `tower::Layer` implementation; wraps services with `MarkdownService`
- **`MarkdownService<S>`** — `tower::Service` implementation; checks `Accept` header, delegates to inner service, then conditionally converts HTML responses to markdown

The future uses a two-phase state machine (`FutureState::Pending` → `FutureState::Converting`) via `pin_project_lite` to handle the async body read during conversion without boxing the common passthrough path.

Key internal functions: `wants_markdown()` (Accept header parsing), `is_html_response()` (Content-Type check), `convert_response()` (body read + htmd conversion + token counting + header rewriting), `append_vary()`.

All tests are inline in `src/lib.rs` and use `tower::ServiceExt::oneshot` with an axum `Router`.
