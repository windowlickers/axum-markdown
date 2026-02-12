use axum::body::{Body, to_bytes};
use bytes::Bytes;
use http::{
    HeaderMap, HeaderValue, Request, Response,
    header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE, VARY},
};
use pin_project_lite::pin_project;
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tower::{Layer, Service};

/// Configuration for the markdown conversion middleware.
#[derive(Debug, Clone)]
pub struct MarkdownConfig {
    /// Maximum HTML body size (in bytes) to attempt conversion on. Default: 10MB.
    pub max_body_size: usize,
    /// Optional value for the `Content-Signal` response header.
    pub content_signal: Option<String>,
}

impl Default for MarkdownConfig {
    fn default() -> Self {
        Self {
            max_body_size: 10 * 1024 * 1024,
            content_signal: Some("ai-train=yes, search=yes, ai-input=yes".to_string()),
        }
    }
}

impl MarkdownConfig {
    /// Create a new default configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the maximum body size for conversion.
    pub fn max_body_size(mut self, size: usize) -> Self {
        self.max_body_size = size;
        self
    }

    /// Set the Content-Signal header value.
    pub fn content_signal(mut self, signal: impl Into<String>) -> Self {
        self.content_signal = Some(signal.into());
        self
    }

    /// Disable the Content-Signal header.
    pub fn no_content_signal(mut self) -> Self {
        self.content_signal = None;
        self
    }
}

/// Tower layer that wraps services with markdown content negotiation.
#[derive(Debug, Clone)]
pub struct MarkdownLayer {
    config: Arc<MarkdownConfig>,
}

impl MarkdownLayer {
    /// Create a new `MarkdownLayer` with default configuration.
    pub fn new() -> Self {
        Self {
            config: Arc::new(MarkdownConfig::default()),
        }
    }

    /// Create a new `MarkdownLayer` with the given configuration.
    pub fn with_config(config: MarkdownConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }
}

impl Default for MarkdownLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Layer<S> for MarkdownLayer {
    type Service = MarkdownService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        MarkdownService {
            inner,
            config: Arc::clone(&self.config),
        }
    }
}

/// Tower service that performs markdown content negotiation.
#[derive(Debug, Clone)]
pub struct MarkdownService<S> {
    inner: S,
    config: Arc<MarkdownConfig>,
}

impl<S> Service<Request<Body>> for MarkdownService<S>
where
    S: Service<Request<Body>, Response = Response<Body>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Send + 'static,
{
    type Response = Response<Body>;
    type Error = S::Error;
    type Future = MarkdownFuture<S::Future, S::Error>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let convert = wants_markdown(req.headers());
        let config = Arc::clone(&self.config);
        let future = self.inner.call(req);

        MarkdownFuture {
            state: FutureState::Pending {
                future,
                convert,
                config,
            },
        }
    }
}

pin_project! {
    /// Future returned by `MarkdownService`.
    pub struct MarkdownFuture<F, E> {
        #[pin]
        state: FutureState<F, E>,
    }
}

pin_project! {
    #[project = FutureStateProj]
    enum FutureState<F, E> {
        Pending {
            #[pin]
            future: F,
            convert: bool,
            config: Arc<MarkdownConfig>,
        },
        Converting {
            #[pin]
            future: Pin<Box<dyn Future<Output = Result<Response<Body>, E>> + Send>>,
        },
    }
}

impl<F, E> Future for MarkdownFuture<F, E>
where
    F: Future<Output = Result<Response<Body>, E>>,
    E: Send + 'static,
{
    type Output = Result<Response<Body>, E>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            let this = self.as_mut().project();
            match this.state.project() {
                FutureStateProj::Pending {
                    future,
                    convert,
                    config,
                } => {
                    let response = match future.poll(cx) {
                        Poll::Ready(Ok(resp)) => resp,
                        Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                        Poll::Pending => return Poll::Pending,
                    };

                    if !*convert || !is_html_response(&response) {
                        // Pass through, but still add Vary: Accept
                        let response = append_vary(response);
                        return Poll::Ready(Ok(response));
                    }

                    let config = Arc::clone(config);
                    let converting = Box::pin(async move {
                        convert_response(response, &config).await
                    });

                    self.as_mut().project().state.set(FutureState::Converting {
                        future: converting,
                    });
                }
                FutureStateProj::Converting { future } => {
                    return future.poll(cx);
                }
            }
        }
    }
}

/// Check if the Accept header explicitly contains `text/markdown`.
fn wants_markdown(headers: &HeaderMap) -> bool {
    headers.get_all(ACCEPT).iter().any(|val| {
        val.to_str().ok().map_or(false, |s| {
            s.split(',')
                .any(|part| part.split(';').next().unwrap_or("").trim() == "text/markdown")
        })
    })
}

/// Check if a response has a `text/html` content type.
fn is_html_response(response: &Response<Body>) -> bool {
    response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map_or(false, |ct| ct.contains("text/html"))
}

/// Append `Accept` to the `Vary` header of a response.
fn append_vary(mut response: Response<Body>) -> Response<Body> {
    let headers = response.headers_mut();

    if let Some(existing) = headers.get(VARY).cloned() {
        if let Ok(s) = existing.to_str() {
            if !s.split(',').any(|p| p.trim().eq_ignore_ascii_case("accept")) {
                let new_val = format!("{}, Accept", s);
                if let Ok(hv) = HeaderValue::from_str(&new_val) {
                    headers.insert(VARY, hv);
                }
            }
        }
    } else {
        headers.insert(VARY, HeaderValue::from_static("Accept"));
    }

    response
}

/// Convert an HTML response to markdown.
async fn convert_response<E>(
    response: Response<Body>,
    config: &MarkdownConfig,
) -> Result<Response<Body>, E> {
    let (mut parts, body) = response.into_parts();

    let body_bytes = match to_bytes(body, config.max_body_size).await {
        Ok(bytes) => bytes,
        Err(_) => {
            // Body too large or read error — return original-ish response
            let response = Response::from_parts(parts, Body::empty());
            return Ok(append_vary(response));
        }
    };

    let html = String::from_utf8_lossy(&body_bytes);
    let markdown = htmd::convert(&html).unwrap_or_else(|_| html.into_owned());

    // Count tokens
    let token_count = tiktoken_rs::o200k_base()
        .map(|bpe| bpe.encode_with_special_tokens(&markdown).len())
        .unwrap_or(0);

    // Update headers
    parts
        .headers
        .insert(CONTENT_TYPE, HeaderValue::from_static("text/markdown; charset=utf-8"));
    parts.headers.remove(CONTENT_LENGTH);

    if let Ok(hv) = HeaderValue::from_str(&token_count.to_string()) {
        parts.headers.insert("x-markdown-tokens", hv);
    }

    if let Some(ref signal) = config.content_signal {
        if let Ok(hv) = HeaderValue::from_str(signal) {
            parts.headers.insert("content-signal", hv);
        }
    }

    let markdown_bytes = Bytes::from(markdown);
    let mut response = Response::from_parts(parts, Body::from(markdown_bytes));
    response = append_vary(response);

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};
    use http::StatusCode;
    use tower::ServiceExt;

    fn html_response() -> &'static str {
        "<html><body><h1>Hello</h1><p>World</p></body></html>"
    }

    fn app() -> Router {
        Router::new()
            .route("/", get(|| async { axum::response::Html(html_response()) }))
            .route("/json", get(|| async {
                axum::Json(serde_json::json!({"key": "value"}))
            }))
            .layer(MarkdownLayer::new())
    }

    #[test]
    fn test_wants_markdown_basic() {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("text/markdown"));
        assert!(wants_markdown(&headers));
    }

    #[test]
    fn test_wants_markdown_with_params() {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("text/markdown; charset=utf-8"));
        assert!(wants_markdown(&headers));
    }

    #[test]
    fn test_wants_markdown_multiple_types() {
        let mut headers = HeaderMap::new();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("text/html, text/markdown, application/json"),
        );
        assert!(wants_markdown(&headers));
    }

    #[test]
    fn test_does_not_want_markdown_html() {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("text/html"));
        assert!(!wants_markdown(&headers));
    }

    #[test]
    fn test_does_not_want_markdown_wildcard() {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("*/*"));
        assert!(!wants_markdown(&headers));
    }

    #[test]
    fn test_does_not_want_markdown_text_wildcard() {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("text/*"));
        assert!(!wants_markdown(&headers));
    }

    #[tokio::test]
    async fn test_html_passthrough_without_accept() {
        let app = app();

        let req = Request::builder()
            .uri("/")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let ct = response.headers().get(CONTENT_TYPE).unwrap().to_str().unwrap();
        assert!(ct.contains("text/html"));

        // Should still have Vary: Accept
        let vary = response.headers().get(VARY).unwrap().to_str().unwrap();
        assert!(vary.contains("Accept"));
    }

    #[tokio::test]
    async fn test_converts_html_to_markdown() {
        let app = app();

        let req = Request::builder()
            .uri("/")
            .header(ACCEPT, "text/markdown")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let ct = response.headers().get(CONTENT_TYPE).unwrap().to_str().unwrap();
        assert_eq!(ct, "text/markdown; charset=utf-8");

        // Should have token count header
        assert!(response.headers().get("x-markdown-tokens").is_some());
        let tokens: usize = response
            .headers()
            .get("x-markdown-tokens")
            .unwrap()
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(tokens > 0);

        // Should have Content-Signal header
        let signal = response.headers().get("content-signal").unwrap().to_str().unwrap();
        assert_eq!(signal, "ai-train=yes, search=yes, ai-input=yes");

        // Should have Vary: Accept
        let vary = response.headers().get(VARY).unwrap().to_str().unwrap();
        assert!(vary.contains("Accept"));

        // Body should be markdown
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let md = String::from_utf8(body.to_vec()).unwrap();
        assert!(md.contains("# Hello"));
        assert!(md.contains("World"));
    }

    #[tokio::test]
    async fn test_non_html_passthrough() {
        let app = app();

        let req = Request::builder()
            .uri("/json")
            .header(ACCEPT, "text/markdown")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let ct = response.headers().get(CONTENT_TYPE).unwrap().to_str().unwrap();
        assert!(ct.contains("application/json"));
    }

    #[tokio::test]
    async fn test_custom_config_no_signal() {
        let config = MarkdownConfig::new().no_content_signal();
        let app = Router::new()
            .route("/", get(|| async { axum::response::Html(html_response()) }))
            .layer(MarkdownLayer::with_config(config));

        let req = Request::builder()
            .uri("/")
            .header(ACCEPT, "text/markdown")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get("content-signal").is_none());
    }
}
