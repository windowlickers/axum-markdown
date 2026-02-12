use axum::{Router, response::Html, routing::get};
use axum_markdown::MarkdownLayer;

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/", get(index))
        .route("/about", get(about))
        .layer(MarkdownLayer::new());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await
        .unwrap();

    println!("Listening on http://127.0.0.1:3000");
    println!();
    println!("Try:");
    println!("  curl http://localhost:3000/          # HTML response");
    println!("  curl -H 'Accept: text/markdown' http://localhost:3000/  # Markdown response");

    axum::serve(listener, app).await.unwrap();
}

async fn index() -> Html<&'static str> {
    Html(
        r#"<!DOCTYPE html>
<html>
<head><title>Home</title></head>
<body>
  <h1>Welcome to axum-markdown</h1>
  <p>This is a demo of the <strong>Markdown for Agents</strong> middleware.</p>
  <h2>Features</h2>
  <ul>
    <li>Automatic HTML to Markdown conversion</li>
    <li>Token counting via <code>x-markdown-tokens</code> header</li>
    <li>Content negotiation based on <code>Accept</code> header</li>
  </ul>
  <p>Visit <a href="/about">the about page</a> for more info.</p>
</body>
</html>"#,
    )
}

async fn about() -> Html<&'static str> {
    Html(
        r#"<!DOCTYPE html>
<html>
<head><title>About</title></head>
<body>
  <h1>About</h1>
  <p>This middleware implements Cloudflare-style content negotiation for AI agents.</p>
  <p>When a client sends <code>Accept: text/markdown</code>, HTML responses are
  automatically converted to markdown with token count headers.</p>
</body>
</html>"#,
    )
}
