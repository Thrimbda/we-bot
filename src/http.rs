use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Request, State, rejection::JsonRejection},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rmcp::transport::{
    StreamableHttpServerConfig,
    streamable_http_server::{session::local::LocalSessionManager, tower::StreamableHttpService},
};
use subtle::ConstantTimeEq;
use tokio_util::sync::CancellationToken;
use tower_http::trace::TraceLayer;
use tracing::error;

use crate::{
    chat::SendMessageInput,
    config::Secret,
    console_auth::ConsoleAuth,
    mcp::NotificationMcp,
    model::{
        BatchItemResponse, BatchNotifyRequest, BatchNotifyResponse, ErrorBody, ErrorEnvelope,
        HealthResponse, NotificationInput, NotifyResponse,
    },
    service::{NotificationService, NotifyError},
    wechat::{IlinkProvider, VerificationCodeInput, WeChatControlError},
};

const MAX_BATCH_SIZE: usize = 50;
const MAX_REQUEST_BODY_BYTES: usize = 256 * 1024;

#[derive(Clone)]
struct AppState {
    service: NotificationService,
    wechat: IlinkProvider,
}

#[derive(Clone)]
struct AuthState {
    token: Secret,
    console: Option<ConsoleAuth>,
}

pub fn build_router(
    service: NotificationService,
    wechat: IlinkProvider,
    api_token: Secret,
    allowed_hosts: Vec<String>,
    cancellation_token: CancellationToken,
    console_auth: Option<ConsoleAuth>,
) -> Router {
    let app_state = AppState {
        service: service.clone(),
        wechat,
    };

    let mcp_service: StreamableHttpService<NotificationMcp, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(NotificationMcp::new(service.clone())),
            Default::default(),
            StreamableHttpServerConfig::default()
                .with_allowed_hosts(allowed_hosts)
                .with_legacy_session_mode(false)
                .with_json_response(true)
                .with_sse_keep_alive(None)
                .with_max_request_body_bytes(MAX_REQUEST_BODY_BYTES)
                .with_cancellation_token(cancellation_token),
        );

    let auth_state = AuthState {
        token: api_token,
        console: console_auth.clone(),
    };
    let console = Router::new()
        .route("/wechat/accounts", get(wechat_accounts))
        .route(
            "/wechat/accounts/{account_id}/messages",
            get(wechat_messages).post(send_wechat_message),
        )
        .with_state(app_state.clone())
        .layer(middleware::from_fn_with_state(
            auth_state.clone(),
            authorize_console,
        ));

    let protected = Router::new()
        .route("/notify", post(notify))
        .route("/notify/batch", post(notify_batch))
        .route("/wechat/status", get(wechat_status))
        .route("/wechat/login", post(start_wechat_login))
        .route("/wechat/login/{login_id}/poll", post(poll_wechat_login))
        .route("/wechat/login/{login_id}/verify", post(verify_wechat_login))
        .nest_service("/mcp", mcp_service)
        .with_state(app_state)
        .layer(middleware::from_fn_with_state(auth_state, authorize));

    Router::new()
        .route("/", get(console_index))
        .route("/assets/console.css", get(console_css))
        .route("/assets/console.js", get(console_js))
        .route("/health", get(health))
        .route("/auth/config", get(console_auth_config))
        .with_state(console_auth)
        .merge(console)
        .merge(protected)
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
        .layer(TraceLayer::new_for_http())
}

async fn console_auth_config(State(auth): State<Option<ConsoleAuth>>) -> Response {
    no_store(
        Json(serde_json::json!({ "mode": if auth.is_some() { "auth_mini" } else { "api_token" } }))
            .into_response(),
    )
}

async fn authorize_console(
    State(state): State<AuthState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if request.headers().contains_key(header::AUTHORIZATION) {
        return authorize(State(state), request, next).await;
    }
    let Some(auth) = &state.console else {
        return ApiError::unauthorized().into_response();
    };
    if !matches!(
        *request.method(),
        axum::http::Method::GET | axum::http::Method::HEAD
    ) && request
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        != Some(auth.origin.as_str())
    {
        return console_auth_error(
            StatusCode::FORBIDDEN,
            "invalid_origin",
            "the request must originate from this console",
        );
    }
    let checked = auth.check(request.headers()).await;
    let mut response = match checked.status {
        StatusCode::NO_CONTENT => no_store(next.run(request).await),
        StatusCode::UNAUTHORIZED => console_auth_error(
            StatusCode::UNAUTHORIZED,
            "login_required",
            "sign in with Auth Mini",
        ),
        StatusCode::FORBIDDEN => console_auth_error(
            StatusCode::FORBIDDEN,
            "access_denied",
            "this Auth Mini user cannot access the console",
        ),
        _ => console_auth_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "auth_unavailable",
            "Auth Mini is temporarily unavailable",
        ),
    };
    for cookie in checked.cookies {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    response
}

fn console_auth_error(status: StatusCode, code: &str, message: &str) -> Response {
    no_store(
        (
            status,
            Json(ErrorEnvelope {
                error: ErrorBody {
                    code: code.to_owned(),
                    message: message.to_owned(),
                },
            }),
        )
            .into_response(),
    )
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        provider: "wechat_ilink",
    })
}

fn console_asset(content_type: &'static str, content: &'static str) -> Response {
    let mut response = no_store(([(header::CONTENT_TYPE, content_type)], content).into_response());
    response.headers_mut().insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(
        "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; font-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'"
    ));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

async fn console_index() -> Response {
    console_asset(
        "text/html; charset=utf-8",
        include_str!("../web/index.html"),
    )
}

async fn console_css() -> Response {
    console_asset(
        "text/css; charset=utf-8",
        include_str!("../web/console.css"),
    )
}

async fn console_js() -> Response {
    console_asset(
        "text/javascript; charset=utf-8",
        include_str!("../web/console.js"),
    )
}

async fn wechat_accounts(State(state): State<AppState>) -> Response {
    no_store(Json(state.wechat.accounts().await).into_response())
}

async fn wechat_messages(
    State(state): State<AppState>,
    Path(account_id): Path<String>,
) -> Result<Response, ApiError> {
    let history = state
        .wechat
        .messages(&account_id)
        .await
        .map_err(NotifyError::Provider)?;
    Ok(no_store(Json(history).into_response()))
}

async fn send_wechat_message(
    State(state): State<AppState>,
    Path(account_id): Path<String>,
    payload: Result<Json<SendMessageInput>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(input) = payload.map_err(ApiError::from_json_rejection)?;
    let text = input.validate().map_err(ApiError::unprocessable)?;
    // Share the REST/MCP rate budget; the provider checks the explicit account again before sending.
    state
        .wechat
        .messages(&account_id)
        .await
        .map_err(NotifyError::Provider)?;
    state.service.acquire_rate_slot().await?;
    let response = state
        .wechat
        .send_message(&account_id, text)
        .await
        .map_err(NotifyError::Provider)?;
    Ok(no_store(Json(response).into_response()))
}

async fn wechat_status(State(state): State<AppState>) -> Response {
    no_store(Json(state.wechat.status().await).into_response())
}

async fn start_wechat_login(State(state): State<AppState>) -> Result<Response, ApiError> {
    let response = state.wechat.start_login().await?;
    Ok(no_store(Json(response).into_response()))
}

async fn poll_wechat_login(
    State(state): State<AppState>,
    Path(login_id): Path<String>,
) -> Result<Response, ApiError> {
    let response = state.wechat.poll_login(&login_id, None).await?;
    Ok(no_store(Json(response).into_response()))
}

async fn verify_wechat_login(
    State(state): State<AppState>,
    Path(login_id): Path<String>,
    payload: Result<Json<VerificationCodeInput>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(input) = payload.map_err(ApiError::from_json_rejection)?;
    let response = state
        .wechat
        .poll_login(&login_id, Some(input.code.trim()))
        .await?;
    Ok(no_store(Json(response).into_response()))
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn notify(
    State(state): State<AppState>,
    payload: Result<Json<NotificationInput>, JsonRejection>,
) -> Result<Json<NotifyResponse>, ApiError> {
    let Json(input) = payload.map_err(ApiError::from_json_rejection)?;
    let response = state.service.notify(input, "rest").await?;
    Ok(Json(response))
}

async fn notify_batch(
    State(state): State<AppState>,
    payload: Result<Json<BatchNotifyRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(batch) = payload.map_err(ApiError::from_json_rejection)?;
    if batch.notifications.is_empty() || batch.notifications.len() > MAX_BATCH_SIZE {
        return Err(ApiError::unprocessable(format!(
            "notifications must contain between 1 and {MAX_BATCH_SIZE} items"
        )));
    }

    let prepared = batch
        .notifications
        .into_iter()
        .map(|input| state.service.prepare(input, "rest"))
        .collect::<Result<Vec<_>, _>>()?;

    let mut has_error = false;
    let mut results = Vec::with_capacity(prepared.len());
    for (index, notification) in prepared.into_iter().enumerate() {
        match state.service.deliver(notification).await {
            Ok(result) => results.push(BatchItemResponse {
                index,
                result: Some(result),
                error: None,
            }),
            Err(error) => {
                has_error = true;
                error!(index, code = error.code(), reason = %error, "batch notification failed");
                results.push(BatchItemResponse {
                    index,
                    result: None,
                    error: Some(ErrorBody {
                        code: error.code().to_owned(),
                        message: error.public_message(),
                    }),
                });
            }
        }
    }

    let status = if has_error {
        StatusCode::MULTI_STATUS
    } else {
        StatusCode::OK
    };
    Ok((status, Json(BatchNotifyResponse { results })).into_response())
}

async fn authorize(State(state): State<AuthState>, request: Request<Body>, next: Next) -> Response {
    let supplied = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));

    let authorized = supplied.is_some_and(|supplied| {
        supplied.len() == state.token.expose().len()
            && supplied
                .as_bytes()
                .ct_eq(state.token.expose().as_bytes())
                .into()
    });
    if authorized {
        return next.run(request).await;
    }

    let mut response = ApiError::unauthorized().into_response();
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    retry_after: Option<u64>,
}

impl ApiError {
    fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            message: "a valid bearer token is required".to_owned(),
            retry_after: None,
        }
    }

    fn unprocessable(message: String) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "invalid_notification",
            message,
            retry_after: None,
        }
    }

    fn from_json_rejection(rejection: JsonRejection) -> Self {
        Self {
            status: rejection.status(),
            code: "invalid_json",
            message: rejection.body_text(),
            retry_after: None,
        }
    }
}

impl From<NotifyError> for ApiError {
    fn from(error: NotifyError) -> Self {
        let status = match &error {
            NotifyError::Provider(crate::provider::ProviderError::AccountNotFound) => {
                StatusCode::NOT_FOUND
            }
            NotifyError::Validation(_) => StatusCode::UNPROCESSABLE_ENTITY,
            NotifyError::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            NotifyError::Provider(error) if error.is_setup_error() => StatusCode::CONFLICT,
            NotifyError::Provider(crate::provider::ProviderError::PayloadTooLarge) => {
                StatusCode::PAYLOAD_TOO_LARGE
            }
            NotifyError::Provider(_) => StatusCode::BAD_GATEWAY,
        };
        if matches!(error, NotifyError::Provider(_)) {
            error!(reason = %error, "notification delivery failed");
        }
        Self {
            status,
            code: error.code(),
            message: error.public_message(),
            retry_after: error.retry_after(),
        }
    }
}

impl From<WeChatControlError> for ApiError {
    fn from(error: WeChatControlError) -> Self {
        let status = if error.is_not_found() {
            StatusCode::NOT_FOUND
        } else if error.is_gone() {
            StatusCode::GONE
        } else if error.is_validation() {
            StatusCode::UNPROCESSABLE_ENTITY
        } else if matches!(error, WeChatControlError::Persistence(_)) {
            StatusCode::INTERNAL_SERVER_ERROR
        } else {
            StatusCode::BAD_GATEWAY
        };
        error!(code = error.code(), reason = %error, "WeChat control request failed");
        Self {
            status,
            code: error.code(),
            message: error.public_message().to_owned(),
            retry_after: None,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (
            self.status,
            Json(ErrorEnvelope {
                error: ErrorBody {
                    code: self.code.to_owned(),
                    message: self.message,
                },
            }),
        )
            .into_response();
        if let Some(seconds) = self.retry_after
            && let Ok(value) = HeaderValue::from_str(&seconds.to_string())
        {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        no_store(response)
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use async_trait::async_trait;
    use axum::{
        Json, Router,
        body::Body,
        http::{Request, StatusCode, header},
        response::IntoResponse,
        routing::post,
    };
    use http_body_util::BodyExt;
    use reqwest::Url;
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;

    use super::{ApiError, build_router};
    use crate::{
        config::Secret,
        model::Notification,
        provider::{NotificationProvider, ProviderError},
        service::{NotificationService, NotifyError},
        wechat::IlinkProvider,
    };

    struct MockProvider;

    #[async_trait]
    impl NotificationProvider for MockProvider {
        fn name(&self) -> &'static str {
            "mock"
        }

        async fn send(&self, _notification: &Notification) -> Result<(), ProviderError> {
            Ok(())
        }
    }

    fn app() -> axum::Router {
        let directory = TempDir::new().unwrap();
        let cancellation = CancellationToken::new();
        let wechat = IlinkProvider::new_for_test(
            Url::parse("http://127.0.0.1:9/").unwrap(),
            directory.path().join("state.json"),
            cancellation.child_token(),
        )
        .unwrap();
        app_with_wechat(wechat, cancellation)
    }

    fn app_with_wechat(wechat: IlinkProvider, cancellation: CancellationToken) -> axum::Router {
        app_with_console_auth(wechat, cancellation, None)
    }

    fn app_with_console_auth(
        wechat: IlinkProvider,
        cancellation: CancellationToken,
        console: Option<crate::console_auth::ConsoleAuth>,
    ) -> axum::Router {
        build_router(
            NotificationService::new(Arc::new(MockProvider), Duration::from_secs(60), 100),
            wechat,
            Secret::new("a".repeat(32)),
            vec!["localhost".to_owned()],
            cancellation,
            console,
        )
    }

    async fn qr_login_response() -> Json<Value> {
        Json(json!({
            "qrcode": "opaque-login-id",
            "qrcode_img_content": "https://example.test/qr-content"
        }))
    }

    async fn json_body(response: axum::response::Response) -> Value {
        let body = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn health_is_public() {
        let response = app()
            .oneshot(Request::get("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["status"], "ok");
        assert_eq!(body["provider"], "wechat_ilink");
    }

    #[tokio::test]
    async fn console_is_public_but_all_conversation_routes_require_authentication() {
        for path in ["/", "/assets/console.css", "/assets/console.js"] {
            let response = app()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            assert!(
                response.headers()[header::CONTENT_SECURITY_POLICY]
                    .to_str()
                    .unwrap()
                    .contains("connect-src 'self'")
            );
        }
        for (method, path) in [
            ("GET", "/wechat/accounts"),
            ("GET", "/wechat/accounts/missing/messages"),
            ("POST", "/wechat/accounts/missing/messages"),
        ] {
            let response = app()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        let response = app()
            .oneshot(
                Request::get("/wechat/accounts")
                    .header(header::AUTHORIZATION, format!("Bearer {}", "a".repeat(32)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(json_body(response).await["accounts"], json!([]));
    }

    #[tokio::test]
    async fn auth_mini_sessions_authorize_only_console_routes_and_enforce_same_origin_writes() {
        use axum::http::{HeaderMap, HeaderValue};
        async fn check(headers: HeaderMap) -> axum::response::Response {
            assert!(headers.get(header::AUTHORIZATION).is_none());
            assert!(headers.get("x-auth-mini-user-id").is_none());
            let cookie = headers.get(header::COOKIE).unwrap().to_str().unwrap();
            assert!(!cookie.contains("other="));
            let status = match cookie {
                "amg_session=valid" => StatusCode::NO_CONTENT,
                "amg_session=denied" => StatusCode::FORBIDDEN,
                "amg_session=expired" => StatusCode::UNAUTHORIZED,
                "amg_session=redirect" => StatusCode::FOUND,
                _ => StatusCode::SERVICE_UNAVAILABLE,
            };
            let mut response = status.into_response();
            response.headers_mut().insert(
                header::SET_COOKIE,
                HeaderValue::from_static(
                    "amg_session=renewed; Path=/; HttpOnly; Secure; SameSite=Lax",
                ),
            );
            response
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gateway = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/auth/check", axum::routing::get(check)),
            )
            .await
            .unwrap();
        });
        let directory = TempDir::new().unwrap();
        let cancel = CancellationToken::new();
        let wechat = IlinkProvider::new_for_test(
            Url::parse("http://127.0.0.1:9/").unwrap(),
            directory.path().join("state.json"),
            cancel.clone(),
        )
        .unwrap();
        let router = app_with_console_auth(
            wechat,
            cancel,
            Some(
                crate::console_auth::ConsoleAuth::new(&gateway, "https://notify.example.com")
                    .unwrap(),
            ),
        );
        let config = router
            .clone()
            .oneshot(Request::get("/auth/config").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(config.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(json_body(config).await["mode"], "auth_mini");
        for (cookie, expected) in [
            ("valid", StatusCode::OK),
            ("denied", StatusCode::FORBIDDEN),
            ("expired", StatusCode::UNAUTHORIZED),
            ("broken", StatusCode::SERVICE_UNAVAILABLE),
            ("redirect", StatusCode::SERVICE_UNAVAILABLE),
        ] {
            let response = router
                .clone()
                .oneshot(
                    Request::get("/wechat/accounts")
                        .header(
                            header::COOKIE,
                            format!("other=private; amg_session={cookie}"),
                        )
                        .header("x-auth-mini-user-id", "forged")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            assert!(
                response.headers().contains_key(header::SET_COOKIE),
                "cookie renewal and clearing must reach the browser"
            );
        }
        let forged = router
            .clone()
            .oneshot(
                Request::get("/wechat/accounts")
                    .header("x-auth-mini-user-id", "forged")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(forged.status(), StatusCode::UNAUTHORIZED);
        let invalid_bearer = router
            .clone()
            .oneshot(
                Request::get("/wechat/accounts")
                    .header(header::AUTHORIZATION, "Bearer invalid")
                    .header(header::COOKIE, "amg_session=valid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid_bearer.status(), StatusCode::UNAUTHORIZED);
        for path in ["/notify", "/mcp", "/wechat/login"] {
            let response = router
                .clone()
                .oneshot(
                    Request::post(path)
                        .header(header::COOKIE, "amg_session=valid")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "browser sessions do not grant machine API access"
            );
        }
        for origin in [
            None,
            Some("https://sibling.example.com"),
            Some("null"),
            Some("https://notify.example.com"),
        ] {
            let mut request = Request::post("/wechat/accounts/missing/messages")
                .header(header::COOKIE, "amg_session=valid")
                .header(header::CONTENT_TYPE, "application/json");
            if let Some(origin) = origin {
                request = request.header(header::ORIGIN, origin);
            }
            let response = router
                .clone()
                .oneshot(request.body(Body::from(r#"{"text":"test"}"#)).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                if origin == Some("https://notify.example.com") {
                    StatusCode::NOT_FOUND
                } else {
                    StatusCode::FORBIDDEN
                }
            );
        }
        let bearer = router
            .oneshot(
                Request::post("/notify")
                    .header(header::AUTHORIZATION, format!("Bearer {}", "a".repeat(32)))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"title":"Test","body":"Hook compatibility"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bearer.status(), StatusCode::OK);
        task.abort();
    }

    #[tokio::test]
    async fn chat_routes_use_explicit_target_and_share_notification_rate_limit() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_url =
            Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let upstream = Router::new().route(
            "/ilink/bot/sendmessage",
            post(|| async { Json(json!({"ret": 0})) }),
        );
        let handle = tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("state.json");
        std::fs::write(&path, json!({
            "version": 1, "bot_token": "private-bot-token", "bot_id": "private-bot-id",
            "user_id": "private-owner", "base_url": upstream_url.as_str(), "context_token": "private-context"
        }).to_string()).unwrap();
        let cancellation = CancellationToken::new();
        let wechat =
            IlinkProvider::new_for_test(upstream_url, path, cancellation.child_token()).unwrap();
        let account_id = wechat.accounts().await.accounts[0].id.clone();
        let router = build_router(
            NotificationService::new(Arc::new(wechat.clone()), Duration::from_secs(60), 1),
            wechat,
            Secret::new("a".repeat(32)),
            vec!["localhost".to_owned()],
            cancellation,
            None,
        );
        let chat_path = format!("/wechat/accounts/{account_id}/messages");
        let request = |path: &str, body: Value| {
            Request::post(path)
                .header(header::AUTHORIZATION, format!("Bearer {}", "a".repeat(32)))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap()
        };
        for text in [" ".to_owned(), "微".repeat(4001)] {
            let invalid = router
                .clone()
                .oneshot(request(&chat_path, json!({"text": text})))
                .await
                .unwrap();
            assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
        }
        let wrong_target = router
            .clone()
            .oneshot(request(
                "/wechat/accounts/wrong/messages",
                json!({"text": "test"}),
            ))
            .await
            .unwrap();
        assert_eq!(wrong_target.status(), StatusCode::NOT_FOUND);
        let accepted = router
            .clone()
            .oneshot(request(&chat_path, json!({"text": "你好 👋"})))
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::OK);
        assert_eq!(accepted.headers()[header::CACHE_CONTROL], "no-store");
        let accepted = json_body(accepted).await;
        assert_eq!(accepted["message"]["account_id"], account_id);
        assert_eq!(accepted["message"]["text"], "你好 👋");
        assert_eq!(accepted["history_saved"], true);
        let limited = router
            .clone()
            .oneshot(request(
                "/notify",
                json!({"title": "Test", "body": "Shared limit"}),
            ))
            .await
            .unwrap();
        assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(limited.headers().contains_key(header::RETRY_AFTER));
        let history = router
            .oneshot(
                Request::get(&chat_path)
                    .header(header::AUTHORIZATION, format!("Bearer {}", "a".repeat(32)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(history.headers()[header::CACHE_CONTROL], "no-store");
        let history = json_body(history).await;
        assert_eq!(history["messages"].as_array().unwrap().len(), 1);
        assert!(!history.to_string().contains("private-"));
        handle.abort();
    }

    #[tokio::test]
    async fn wechat_status_is_protected_and_redacted() {
        let unauthorized = app()
            .oneshot(Request::get("/wechat/status").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let authorized = app()
            .oneshot(
                Request::get("/wechat/status")
                    .header(header::AUTHORIZATION, format!("Bearer {}", "a".repeat(32)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(authorized.status(), StatusCode::OK);
        assert_eq!(authorized.headers()[header::CACHE_CONTROL], "no-store");
        let body = json_body(authorized).await;
        assert_eq!(body["state"], "not_linked");
        assert!(body.get("bot_token").is_none());
    }

    #[tokio::test]
    async fn wechat_login_route_is_authenticated_and_not_cached() {
        let upstream = Router::new().route("/ilink/bot/get_bot_qrcode", post(qr_login_response));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_url =
            Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let upstream_handle = tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });
        let directory = TempDir::new().unwrap();
        let cancellation = CancellationToken::new();
        let wechat = IlinkProvider::new_for_test(
            upstream_url,
            directory.path().join("state.json"),
            cancellation.child_token(),
        )
        .unwrap();

        let response = app_with_wechat(wechat, cancellation)
            .oneshot(
                Request::post("/wechat/login")
                    .header(header::AUTHORIZATION, format!("Bearer {}", "a".repeat(32)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let body = json_body(response).await;
        assert_eq!(body["qr_content"], "https://example.test/qr-content");
        assert_eq!(body["status"], "waiting_for_scan");
        assert!(body.get("bot_token").is_none());

        upstream_handle.abort();
    }

    #[tokio::test]
    async fn notify_requires_bearer_token() {
        let response = app()
            .oneshot(
                Request::post("/notify")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"title":"Done","body":"Complete"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(json_body(response).await["error"]["code"], "unauthorized");
    }

    #[tokio::test]
    async fn unlinked_wechat_maps_to_conflict_without_secret_details() {
        let response =
            ApiError::from(NotifyError::Provider(ProviderError::NotLinked)).into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = json_body(response).await;
        assert_eq!(body["error"]["code"], "wechat_not_linked");
    }

    #[tokio::test]
    async fn notify_delivers_with_valid_token() {
        let response = app()
            .oneshot(
                Request::post("/notify")
                    .header(header::AUTHORIZATION, format!("Bearer {}", "a".repeat(32)))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"title":"Done","body":"Complete"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_body(response).await["status"], "sent");
    }

    #[tokio::test]
    async fn mcp_exposes_send_notification_tool() {
        let response = app()
            .oneshot(
                Request::post("/mcp")
                    .header(header::HOST, "localhost")
                    .header(header::AUTHORIZATION, format!("Bearer {}", "a".repeat(32)))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::ACCEPT, "application/json, text/event-stream")
                    .body(Body::from(
                        json!({
                            "jsonrpc": "2.0",
                            "id": 1,
                            "method": "tools/list",
                            "params": {}
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["result"]["tools"][0]["name"], "send_notification");
    }

    #[tokio::test]
    async fn mcp_tool_delivers_a_notification() {
        let response = app()
            .oneshot(
                Request::post("/mcp")
                    .header(header::HOST, "localhost")
                    .header(header::AUTHORIZATION, format!("Bearer {}", "a".repeat(32)))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::ACCEPT, "application/json, text/event-stream")
                    .body(Body::from(
                        json!({
                            "jsonrpc": "2.0",
                            "id": 2,
                            "method": "tools/call",
                            "params": {
                                "name": "send_notification",
                                "arguments": {
                                    "title": "Done",
                                    "body": "Complete"
                                }
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["result"]["structuredContent"]["status"], "sent");
        assert_eq!(body["result"]["isError"], false);
    }

    #[tokio::test]
    async fn mcp_rejects_an_untrusted_host() {
        let response = app()
            .oneshot(
                Request::post("/mcp")
                    .header(header::HOST, "attacker.example")
                    .header(header::AUTHORIZATION, format!("Bearer {}", "a".repeat(32)))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::ACCEPT, "application/json, text/event-stream")
                    .body(Body::from(
                        json!({
                            "jsonrpc": "2.0",
                            "id": 1,
                            "method": "tools/list",
                            "params": {}
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
