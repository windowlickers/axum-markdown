#![cfg_attr(docsrs, feature(doc_auto_cfg))]
#![doc = include_str!("../README.md")]

use axum::body::{to_bytes, Body};
use bytes::Bytes;
use http::{
    header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE, VARY},
    HeaderMap, HeaderValue, Request, Response,
};
use pin_project_lite::pin_project;
use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tower::{Layer, Service};

// ── Traits ──────────────────────────────────────────────────────

/// Error type returned by [`HtmlConverter::convert`].
pub type ConvertError = Box<dyn std::error::Error + Send + Sync>;

/// Converts an HTML string to markdown.
///
/// The default implementation ([`HtmdConverter`]) delegates to
/// [`htmd::convert`].
pub trait HtmlConverter: Send + Sync {
    /// Convert `html` to a markdown string.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTML cannot be converted.
    fn convert(&self, html: &str) -> Result<String, ConvertError>;
}

/// Counts tokens in a text string.
///
/// When the `tokens` feature is enabled (default), the built-in
/// [`TiktokenCounter`] uses the `o200k_base` tokenizer.
pub trait TokenCounter: Send + Sync {
    /// Return the token count for `text`.
    fn count_tokens(&self, text: &str) -> usize;
}

// ── Default implementations ─────────────────────────────────────

/// HTML-to-markdown converter backed by [`htmd`].
#[derive(Debug, Clone, Copy)]
pub struct HtmdConverter;

impl HtmlConverter for HtmdConverter {
    fn convert(&self, html: &str) -> Result<String, ConvertError> {
        htmd::convert(html).map_err(Into::into)
    }
}

/// Token counter using [`tiktoken_rs`] with the `o200k_base`
/// tokenizer.
///
/// Only available when the `tokens` feature is enabled (default).
#[cfg(feature = "tokens")]
#[derive(Debug, Clone, Copy)]
pub struct TiktokenCounter;

#[cfg(feature = "tokens")]
#[allow(clippy::expect_used)] // No meaningful recovery if tokenizer fails
static BPE: std::sync::LazyLock<tiktoken_rs::CoreBPE> = std::sync::LazyLock::new(|| {
    tiktoken_rs::o200k_base().expect("failed to initialize o200k_base tokenizer")
});

#[cfg(feature = "tokens")]
impl TokenCounter for TiktokenCounter {
    fn count_tokens(&self, text: &str) -> usize {
        BPE.encode_with_special_tokens(text).len()
    }
}

// ── WantsMarkdown extractor ─────────────────────────────────────

/// Infallible axum extractor indicating whether the client sent
/// `Accept: text/markdown`.
///
/// ```rust
/// # use axum_markdown::WantsMarkdown;
/// # use axum::response::IntoResponse;
/// async fn handler(
///     WantsMarkdown(wants_md): WantsMarkdown,
/// ) -> impl IntoResponse {
///     if wants_md { "markdown" } else { "html" }
/// }
/// ```
#[derive(Debug, Clone, Copy)]
pub struct WantsMarkdown(pub bool);

impl<S> axum::extract::FromRequestParts<S> for WantsMarkdown
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self(wants_markdown(&parts.headers)))
    }
}

// ── Configuration ───────────────────────────────────────────────

type SkipPredicate = Arc<dyn Fn(&Request<Body>) -> bool + Send + Sync>;

/// Configuration for the markdown conversion middleware.
///
/// Use the builder methods to customise behaviour:
///
/// ```rust
/// use axum_markdown::MarkdownConfig;
///
/// let config = MarkdownConfig::new()
///     .max_body_size(2 * 1024 * 1024)
///     .content_signal("ai-train=no");
/// ```
#[derive(Clone)]
#[non_exhaustive]
pub struct MarkdownConfig {
    max_body_size: usize,
    content_signal: Option<String>,
    converter: Arc<dyn HtmlConverter>,
    token_counter: Option<Arc<dyn TokenCounter>>,
    skip_predicate: Option<SkipPredicate>,
}

impl fmt::Debug for MarkdownConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MarkdownConfig")
            .field("max_body_size", &self.max_body_size)
            .field("content_signal", &self.content_signal)
            .field("converter", &"<dyn HtmlConverter>")
            .field(
                "token_counter",
                &self.token_counter.as_ref().map(|_| "<dyn TokenCounter>"),
            )
            .field(
                "skip_predicate",
                &self.skip_predicate.as_ref().map(|_| "<fn>"),
            )
            .finish()
    }
}

impl Default for MarkdownConfig {
    fn default() -> Self {
        Self {
            max_body_size: 1024 * 1024,
            content_signal: Some("ai-train=yes, search=yes, ai-input=yes".to_string()),
            converter: Arc::new(HtmdConverter),
            #[cfg(feature = "tokens")]
            token_counter: Some(Arc::new(TiktokenCounter)),
            #[cfg(not(feature = "tokens"))]
            token_counter: None,
            skip_predicate: None,
        }
    }
}

impl MarkdownConfig {
    /// Create a new default configuration.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the maximum body size (in bytes) for conversion.
    /// Default: 1 MB.
    #[must_use]
    pub const fn max_body_size(mut self, size: usize) -> Self {
        self.max_body_size = size;
        self
    }

    /// Set the `Content-Signal` response header value.
    #[must_use]
    pub fn content_signal(mut self, signal: impl Into<String>) -> Self {
        self.content_signal = Some(signal.into());
        self
    }

    /// Disable the `Content-Signal` header.
    #[must_use]
    pub fn no_content_signal(mut self) -> Self {
        self.content_signal = None;
        self
    }

    /// Use a custom HTML-to-markdown converter.
    #[must_use]
    pub fn converter(mut self, converter: impl HtmlConverter + 'static) -> Self {
        self.converter = Arc::new(converter);
        self
    }

    /// Use a custom token counter.
    #[must_use]
    pub fn token_counter(mut self, counter: impl TokenCounter + 'static) -> Self {
        self.token_counter = Some(Arc::new(counter));
        self
    }

    /// Disable token counting (omits `x-markdown-tokens` header).
    #[must_use]
    pub fn no_token_counter(mut self) -> Self {
        self.token_counter = None;
        self
    }

    /// Skip markdown conversion for requests matching `predicate`.
    ///
    /// The predicate receives the incoming request and returns
    /// `true` to skip conversion. The response still gets
    /// `Vary: Accept`.
    ///
    /// ```rust
    /// use axum_markdown::MarkdownConfig;
    ///
    /// let config = MarkdownConfig::new()
    ///     .skip_when(|req| req.uri().path().starts_with("/api"));
    /// ```
    #[must_use]
    pub fn skip_when(
        mut self,
        predicate: impl Fn(&Request<Body>) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.skip_predicate = Some(Arc::new(predicate));
        self
    }
}

impl From<MarkdownConfig> for MarkdownLayer {
    fn from(config: MarkdownConfig) -> Self {
        Self::with_config(config)
    }
}

// ── Layer ───────────────────────────────────────────────────────

/// Tower layer that wraps services with markdown content
/// negotiation.
#[derive(Debug, Clone)]
pub struct MarkdownLayer {
    config: Arc<MarkdownConfig>,
}

impl MarkdownLayer {
    /// Create a new `MarkdownLayer` with default configuration.
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: Arc::new(MarkdownConfig::default()),
        }
    }

    /// Create a new `MarkdownLayer` with the given configuration.
    #[must_use]
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

// ── Service ─────────────────────────────────────────────────────

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
        let skipped = self.config.skip_predicate.as_ref().is_some_and(|p| p(&req));
        let convert = !skipped && wants_markdown(req.headers());
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

// ── Future ──────────────────────────────────────────────────────

pin_project! {
    /// Future returned by [`MarkdownService`].
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
                        #[cfg(feature = "tracing")]
                        tracing::trace!("passthrough (no conversion)");
                        return Poll::Ready(Ok(append_vary(response)));
                    }

                    let config = Arc::clone(config);
                    let fut = async move { convert_response(response, &config).await };
                    #[cfg(feature = "tracing")]
                    let fut = tracing::Instrument::instrument(
                        fut,
                        tracing::debug_span!("markdown_conversion"),
                    );
                    let converting = Box::pin(fut);

                    self.as_mut()
                        .project()
                        .state
                        .set(FutureState::Converting { future: converting });
                }
                FutureStateProj::Converting { future } => {
                    return future.poll(cx);
                }
            }
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────

/// Check if the Accept header explicitly contains `text/markdown`.
fn wants_markdown(headers: &HeaderMap) -> bool {
    headers.get_all(ACCEPT).iter().any(|val| {
        val.to_str().ok().is_some_and(|s| {
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
        .is_some_and(|ct| ct.contains("text/html"))
}

/// Append `Accept` to the `Vary` header of a response.
fn append_vary(mut response: Response<Body>) -> Response<Body> {
    let headers = response.headers_mut();

    let existing_values: Vec<String> = headers
        .get_all(VARY)
        .iter()
        .filter_map(|v| v.to_str().ok().map(String::from))
        .collect();

    if existing_values.is_empty() {
        headers.insert(VARY, HeaderValue::from_static("Accept"));
    } else {
        let already_has_accept = existing_values.iter().any(|s| {
            s.split(',')
                .any(|p| p.trim().eq_ignore_ascii_case("accept"))
        });

        let combined = existing_values.join(", ");
        let new_val = if already_has_accept {
            combined
        } else {
            format!("{combined}, Accept")
        };

        if let Ok(hv) = HeaderValue::from_str(&new_val) {
            headers.insert(VARY, hv);
        }
    }

    response
}

/// Convert an HTML response to markdown.
async fn convert_response<E>(
    response: Response<Body>,
    config: &MarkdownConfig,
) -> Result<Response<Body>, E> {
    #[cfg(feature = "tracing")]
    let start = std::time::Instant::now();

    let (mut parts, body) = response.into_parts();

    // Original body is consumed; cannot forward
    let Ok(body_bytes) = to_bytes(body, config.max_body_size).await else {
        #[cfg(feature = "tracing")]
        tracing::warn!("markdown conversion failed: body too large or unreadable");
        let mut response = Response::new(Body::from(
            "Markdown conversion failed: \
             response body too large or unreadable",
        ));
        *response.status_mut() = http::StatusCode::BAD_GATEWAY;
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        return Ok(append_vary(response));
    };

    let html = String::from_utf8_lossy(&body_bytes);
    // 502 rather than serving raw HTML as text/markdown
    let Ok(markdown) = config.converter.convert(&html) else {
        #[cfg(feature = "tracing")]
        tracing::warn!("markdown conversion failed: converter error");
        let mut response = Response::new(Body::from(
            "Markdown conversion failed: \
             unable to convert HTML to markdown",
        ));
        *response.status_mut() = http::StatusCode::BAD_GATEWAY;
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        return Ok(append_vary(response));
    };

    let token_count = config
        .token_counter
        .as_ref()
        .map(|c| c.count_tokens(&markdown));

    parts.headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/markdown; charset=utf-8"),
    );

    let markdown_bytes = Bytes::from(markdown);

    if let Ok(hv) = HeaderValue::from_str(&markdown_bytes.len().to_string()) {
        parts.headers.insert(CONTENT_LENGTH, hv);
    }

    if let Some(count) = token_count {
        if let Ok(hv) = HeaderValue::from_str(&count.to_string()) {
            parts.headers.insert("x-markdown-tokens", hv);
        }
    }

    if let Some(ref signal) = config.content_signal {
        if let Ok(hv) = HeaderValue::from_str(signal) {
            parts.headers.insert("content-signal", hv);
        }
    }

    #[cfg(feature = "tracing")]
    {
        let body_size = body_bytes.len();
        let md_size = markdown_bytes.len();
        let tokens = token_count.unwrap_or(0);
        let duration_ms = start.elapsed().as_millis();
        tracing::debug!(
            body_size,
            markdown_size = md_size,
            token_count = tokens,
            duration_ms,
            "markdown conversion complete"
        );
    }

    let mut response = Response::from_parts(parts, Body::from(markdown_bytes));
    response = append_vary(response);

    Ok(response)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::{routing::get, Router};
    use http::StatusCode;
    use tower::ServiceExt;

    fn html_response() -> &'static str {
        "<html><body><h1>Hello</h1><p>World</p></body></html>"
    }

    fn app() -> Router {
        Router::new()
            .route("/", get(|| async { axum::response::Html(html_response()) }))
            .route(
                "/json",
                get(|| async { axum::Json(serde_json::json!({"key": "value"})) }),
            )
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
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("text/markdown; charset=utf-8"),
        );
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

        let req = Request::builder().uri("/").body(Body::empty()).unwrap();

        let response = app.oneshot(req).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let ct = response
            .headers()
            .get(CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(ct.contains("text/html"));

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

        let ct = response
            .headers()
            .get(CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(ct, "text/markdown; charset=utf-8");

        #[cfg(feature = "tokens")]
        {
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
        }
        #[cfg(not(feature = "tokens"))]
        assert!(response.headers().get("x-markdown-tokens").is_none());

        let signal = response
            .headers()
            .get("content-signal")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(signal, "ai-train=yes, search=yes, ai-input=yes");

        let vary = response.headers().get(VARY).unwrap().to_str().unwrap();
        assert!(vary.contains("Accept"));

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
        let ct = response
            .headers()
            .get(CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(ct.contains("application/json"));
    }

    #[tokio::test]
    async fn test_body_too_large_returns_502() {
        let config = MarkdownConfig::new().max_body_size(10);
        let app = Router::new()
            .route(
                "/",
                get(|| async {
                    axum::response::Html(
                        "<html><body><h1>This body is \
                         definitely larger than 10 bytes\
                         </h1></body></html>",
                    )
                }),
            )
            .layer(MarkdownLayer::with_config(config));

        let req = Request::builder()
            .uri("/")
            .header(ACCEPT, "text/markdown")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let ct = response
            .headers()
            .get(CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(ct.contains("text/plain"));
        let vary = response.headers().get(VARY).unwrap().to_str().unwrap();
        assert!(vary.contains("Accept"));

        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("too large"));
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

    #[test]
    fn test_append_vary_preserves_multiple_vary_headers() {
        let mut response = Response::builder()
            .status(StatusCode::OK)
            .body(Body::empty())
            .unwrap();
        response
            .headers_mut()
            .append(VARY, HeaderValue::from_static("Cookie"));
        response
            .headers_mut()
            .append(VARY, HeaderValue::from_static("Accept-Encoding"));

        let response = append_vary(response);

        let vary = response.headers().get(VARY).unwrap().to_str().unwrap();
        assert!(
            vary.contains("Cookie"),
            "Vary should contain Cookie, got: {vary}"
        );
        assert!(
            vary.contains("Accept-Encoding"),
            "Vary should contain Accept-Encoding, got: {vary}"
        );
        assert!(
            vary.contains("Accept"),
            "Vary should contain Accept, got: {vary}"
        );
    }

    #[test]
    fn test_append_vary_multiple_headers_already_has_accept() {
        let mut response = Response::builder()
            .status(StatusCode::OK)
            .body(Body::empty())
            .unwrap();
        response
            .headers_mut()
            .append(VARY, HeaderValue::from_static("Cookie"));
        response
            .headers_mut()
            .append(VARY, HeaderValue::from_static("Accept"));

        let response = append_vary(response);

        let vary = response.headers().get(VARY).unwrap().to_str().unwrap();
        assert!(
            vary.contains("Cookie"),
            "Vary should contain Cookie, got: {vary}"
        );
        let accept_count = vary
            .split(',')
            .filter(|p| p.trim().eq_ignore_ascii_case("accept"))
            .count();
        assert_eq!(
            accept_count, 1,
            "Accept should appear exactly once, got: {vary}"
        );
    }

    // ── New tests ───────────────────────────────────────────────

    struct UppercaseConverter;

    impl HtmlConverter for UppercaseConverter {
        fn convert(&self, html: &str) -> Result<String, ConvertError> {
            Ok(html.to_uppercase())
        }
    }

    #[tokio::test]
    async fn test_custom_converter() {
        let config = MarkdownConfig::new()
            .converter(UppercaseConverter)
            .no_token_counter();
        let app = Router::new()
            .route(
                "/",
                get(|| async { axum::response::Html("<h1>hello</h1>") }),
            )
            .layer(MarkdownLayer::with_config(config));

        let req = Request::builder()
            .uri("/")
            .header(ACCEPT, "text/markdown")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let ct = response
            .headers()
            .get(CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(ct, "text/markdown; charset=utf-8");

        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert_eq!(text, "<H1>HELLO</H1>");
    }

    struct ConstantCounter(usize);

    impl TokenCounter for ConstantCounter {
        fn count_tokens(&self, _text: &str) -> usize {
            self.0
        }
    }

    #[tokio::test]
    async fn test_custom_token_counter() {
        let config = MarkdownConfig::new().token_counter(ConstantCounter(42));
        let app = Router::new()
            .route("/", get(|| async { axum::response::Html(html_response()) }))
            .layer(MarkdownLayer::with_config(config));

        let req = Request::builder()
            .uri("/")
            .header(ACCEPT, "text/markdown")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();

        let tokens: usize = response
            .headers()
            .get("x-markdown-tokens")
            .unwrap()
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(tokens, 42);
    }

    #[tokio::test]
    async fn test_no_token_counter_omits_header() {
        let config = MarkdownConfig::new().no_token_counter();
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
        assert!(response.headers().get("x-markdown-tokens").is_none());
    }

    #[tokio::test]
    async fn test_skip_when_skips_matching_paths() {
        let config = MarkdownConfig::new().skip_when(|req| req.uri().path().starts_with("/api"));
        let app = Router::new()
            .route(
                "/api/data",
                get(|| async { axum::response::Html(html_response()) }),
            )
            .route(
                "/page",
                get(|| async { axum::response::Html(html_response()) }),
            )
            .layer(MarkdownLayer::with_config(config));

        // /api/data should be skipped
        let req = Request::builder()
            .uri("/api/data")
            .header(ACCEPT, "text/markdown")
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        let ct = response
            .headers()
            .get(CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(ct.contains("text/html"), "skipped path should remain HTML");
        let vary = response.headers().get(VARY).unwrap().to_str().unwrap();
        assert!(vary.contains("Accept"), "Vary should still be set");

        // /page should be converted
        let req = Request::builder()
            .uri("/page")
            .header(ACCEPT, "text/markdown")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        let ct = response
            .headers()
            .get(CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(ct, "text/markdown; charset=utf-8");
    }

    #[tokio::test]
    async fn test_wants_markdown_extractor() {
        let app = Router::new().route(
            "/",
            get(|WantsMarkdown(wants_md): WantsMarkdown| async move {
                if wants_md {
                    "yes"
                } else {
                    "no"
                }
            }),
        );

        // Without Accept: text/markdown
        let req = Request::builder().uri("/").body(Body::empty()).unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"no");

        // With Accept: text/markdown
        let req = Request::builder()
            .uri("/")
            .header(ACCEPT, "text/markdown")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"yes");
    }

    #[tokio::test]
    async fn test_content_length_set_on_conversion() {
        let app = Router::new()
            .route("/", get(|| async { axum::response::Html(html_response()) }))
            .layer(MarkdownLayer::new());

        let req = Request::builder()
            .uri("/")
            .header(ACCEPT, "text/markdown")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();

        let content_length: usize = response
            .headers()
            .get(CONTENT_LENGTH)
            .unwrap()
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        assert_eq!(content_length, body.len());
    }

    #[tokio::test]
    async fn test_from_config_for_layer() {
        let config = MarkdownConfig::new().no_content_signal();
        let layer: MarkdownLayer = config.into();
        let app = Router::new()
            .route("/", get(|| async { axum::response::Html(html_response()) }))
            .layer(layer);

        let req = Request::builder()
            .uri("/")
            .header(ACCEPT, "text/markdown")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let ct = response
            .headers()
            .get(CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(ct, "text/markdown; charset=utf-8");
        assert!(response.headers().get("content-signal").is_none());
    }
}
