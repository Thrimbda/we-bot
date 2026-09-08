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
    config::Secret,
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
}

pub fn build_router(
    service: NotificationService,
    wechat: IlinkProvider,
    api_token: Secret,
    allowed_hosts: Vec<String>,
    cancellation_token: CancellationToken,
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

    let protected = Router::new()
        .route("/notify", post(notify))
        .route("/notify/batch", post(notify_batch))
        .route("/wechat/status", get(wechat_status))
        .route("/wechat/login", post(start_wechat_login))
        .route("/wechat/login/{login_id}/poll", post(poll_wechat_login))
        .route("/wechat/login/{login_id}/verify", post(verify_wechat_login))
        .nest_service("/mcp", mcp_service)
        .with_state(app_state)
        .layer(middleware::from_fn_with_state(
            AuthState { token: api_token },
            authorize,
        ));

    Router::new()
        .route("/health", get(health))
        .merge(protected)
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
        .layer(TraceLayer::new_for_http())
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        provider: "wechat_ilink",
    })
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
        response
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
        build_router(
            NotificationService::new(Arc::new(MockProvider), Duration::from_secs(60), 100),
            wechat,
            Secret::new("a".repeat(32)),
            vec!["localhost".to_owned()],
            cancellation,
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
