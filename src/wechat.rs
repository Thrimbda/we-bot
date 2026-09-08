use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result as AnyResult, bail};
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{Client, RequestBuilder, Response, Url};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::{
    model::Notification,
    provider::{NotificationProvider, ProviderError},
};

const DEFAULT_BASE_URL: &str = "https://ilinkai.weixin.qq.com/";
const ILINK_APP_ID: &str = "bot";
// Wire compatibility baseline from Tencent's openclaw-weixin 2.4.8 client.
const ILINK_APP_CLIENT_VERSION: &str = "132104";
const ILINK_CHANNEL_VERSION: &str = "2.4.8";
const BOT_TYPE: &str = "3";
const LOGIN_TTL: Duration = Duration::from_secs(5 * 60);
const QR_POLL_TIMEOUT: Duration = Duration::from_secs(40);
const SEND_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_TEXT_CHARS: usize = 4_000;
const MAX_QR_FIELD_BYTES: usize = 8_192;
const STALE_TOKEN_CODE: i64 = -14;

#[derive(Clone)]
pub struct IlinkProvider {
    inner: Arc<Inner>,
}

struct Inner {
    client: Client,
    default_base_url: Url,
    allow_insecure_base_url: bool,
    state_path: PathBuf,
    state: RwLock<PersistedState>,
    login: Mutex<Option<LoginSession>>,
    monitor_cancel: Mutex<Option<CancellationToken>>,
    monitor_generation: AtomicU64,
    monitor_running: AtomicBool,
    auth_stale: AtomicBool,
    shutdown: CancellationToken,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedState {
    #[serde(default = "state_version")]
    version: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bot_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base_url: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    get_updates_buf: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_token: Option<String>,
}

fn state_version() -> u8 {
    1
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            version: state_version(),
            bot_token: None,
            bot_id: None,
            user_id: None,
            base_url: None,
            get_updates_buf: String::new(),
            context_token: None,
        }
    }
}

impl PersistedState {
    fn linked(&self) -> bool {
        self.bot_token.is_some() && self.bot_id.is_some() && self.user_id.is_some()
    }

    fn ready(&self) -> bool {
        self.linked() && self.context_token.is_some()
    }
}

#[derive(Clone)]
struct LoginSession {
    id: String,
    qrcode: String,
    base_url: Url,
    created_at: Instant,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WeChatConnectionState {
    NotLinked,
    WaitingForMessage,
    Ready,
    RelinkRequired,
}

#[derive(Clone, Debug, Serialize)]
pub struct WeChatStatus {
    pub state: WeChatConnectionState,
    pub monitor_running: bool,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginStatus {
    WaitingForScan,
    Scanned,
    VerificationRequired,
    Linked,
    AlreadyLinked,
    Expired,
    VerificationBlocked,
}

#[derive(Clone, Serialize)]
pub struct StartLoginResponse {
    pub login_id: String,
    pub qr_content: String,
    pub expires_in_seconds: u64,
    pub status: LoginStatus,
}

#[derive(Clone, Debug, Serialize)]
pub struct PollLoginResponse {
    pub status: LoginStatus,
    pub linked: bool,
    pub ready: bool,
    pub message: &'static str,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationCodeInput {
    pub code: String,
}

#[derive(Debug, Error)]
pub enum WeChatControlError {
    #[error("login session was not found")]
    LoginNotFound,
    #[error("login session expired")]
    LoginExpired,
    #[error("verification code must contain 1 to 12 digits")]
    InvalidVerificationCode,
    #[error("iLink control request failed")]
    Upstream(#[source] ProviderError),
    #[error("iLink returned an unsafe redirect host")]
    UnsafeRedirect,
    #[error("failed to persist WeChat credentials")]
    Persistence(#[source] StateWriteError),
}

impl WeChatControlError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::LoginNotFound => "login_not_found",
            Self::LoginExpired => "login_expired",
            Self::InvalidVerificationCode => "invalid_verification_code",
            Self::UnsafeRedirect => "unsafe_ilink_redirect",
            Self::Upstream(_) => "ilink_unavailable",
            Self::Persistence(_) => "state_persistence_failed",
        }
    }

    pub fn public_message(&self) -> &'static str {
        match self {
            Self::LoginNotFound => "the WeChat login session was not found",
            Self::LoginExpired => "the WeChat login QR code expired; start a new login",
            Self::InvalidVerificationCode => "verification code must contain 1 to 12 digits",
            Self::UnsafeRedirect => "iLink returned an untrusted redirect",
            Self::Upstream(_) => "WeChat iLink login is temporarily unavailable",
            Self::Persistence(_) => "the server could not save WeChat credentials",
        }
    }

    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::LoginNotFound)
    }

    pub fn is_gone(&self) -> bool {
        matches!(self, Self::LoginExpired)
    }

    pub fn is_validation(&self) -> bool {
        matches!(self, Self::InvalidVerificationCode)
    }
}

#[derive(Debug, Error)]
pub enum StateWriteError {
    #[error("state serialization failed")]
    Serialize(#[source] serde_json::Error),
    #[error("state I/O failed")]
    Io(#[source] std::io::Error),
}

#[derive(Deserialize)]
struct QrResponse {
    qrcode: String,
    qrcode_img_content: String,
}

#[derive(Deserialize)]
struct QrStatusResponse {
    status: String,
    #[serde(default)]
    bot_token: Option<String>,
    #[serde(default)]
    ilink_bot_id: Option<String>,
    #[serde(default)]
    ilink_user_id: Option<String>,
    #[serde(default)]
    baseurl: Option<String>,
    #[serde(default)]
    redirect_host: Option<String>,
}

#[derive(Deserialize)]
struct UpdatesResponse {
    #[serde(default)]
    ret: Option<i64>,
    #[serde(default)]
    errcode: Option<i64>,
    #[serde(default)]
    msgs: Vec<InboundMessage>,
    #[serde(default)]
    get_updates_buf: Option<String>,
    #[serde(default)]
    longpolling_timeout_ms: Option<u64>,
}

#[derive(Deserialize)]
struct InboundMessage {
    #[serde(default)]
    from_user_id: Option<String>,
    #[serde(default)]
    context_token: Option<String>,
}

#[derive(Deserialize)]
struct SendResponse {
    #[serde(default)]
    ret: Option<i64>,
    #[serde(default)]
    errcode: Option<i64>,
}

impl IlinkProvider {
    pub fn new(state_path: PathBuf, shutdown: CancellationToken) -> AnyResult<Self> {
        Self::new_inner(
            Url::parse(DEFAULT_BASE_URL).expect("default iLink URL is valid"),
            state_path,
            shutdown,
            false,
        )
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(
        base_url: Url,
        state_path: PathBuf,
        shutdown: CancellationToken,
    ) -> AnyResult<Self> {
        Self::new_inner(base_url, state_path, shutdown, true)
    }

    fn new_inner(
        base_url: Url,
        state_path: PathBuf,
        shutdown: CancellationToken,
        allow_insecure_base_url: bool,
    ) -> AnyResult<Self> {
        if !allow_insecure_base_url && !is_trusted_ilink_url(&base_url) {
            bail!("the configured iLink base URL is not trusted");
        }
        let state = load_state(&state_path, allow_insecure_base_url, &base_url)?;
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .user_agent(concat!("we-bot/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("failed to create iLink HTTP client")?;
        Ok(Self {
            inner: Arc::new(Inner {
                client,
                default_base_url: base_url,
                allow_insecure_base_url,
                state_path,
                state: RwLock::new(state),
                login: Mutex::new(None),
                monitor_cancel: Mutex::new(None),
                monitor_generation: AtomicU64::new(0),
                monitor_running: AtomicBool::new(false),
                auth_stale: AtomicBool::new(false),
                shutdown,
            }),
        })
    }

    pub async fn start(&self) {
        self.restart_monitor().await;
    }

    pub async fn status(&self) -> WeChatStatus {
        let state = self.inner.state.read().await;
        let connection = if !state.linked() {
            WeChatConnectionState::NotLinked
        } else if self.inner.auth_stale.load(Ordering::Acquire) {
            WeChatConnectionState::RelinkRequired
        } else if state.ready() {
            WeChatConnectionState::Ready
        } else {
            WeChatConnectionState::WaitingForMessage
        };
        WeChatStatus {
            state: connection,
            monitor_running: self.inner.monitor_running.load(Ordering::Acquire),
        }
    }

    pub async fn start_login(&self) -> Result<StartLoginResponse, WeChatControlError> {
        let local_tokens = self
            .inner
            .state
            .read()
            .await
            .bot_token
            .clone()
            .into_iter()
            .collect::<Vec<_>>();
        let url = self
            .inner
            .default_base_url
            .join(&format!("ilink/bot/get_bot_qrcode?bot_type={BOT_TYPE}"))
            .map_err(|_| WeChatControlError::Upstream(ProviderError::InvalidResponse))?;
        let response = self
            .post_json(
                url,
                None,
                &json!({ "local_token_list": local_tokens }),
                SEND_TIMEOUT,
            )
            .await
            .map_err(WeChatControlError::Upstream)?;
        let qr = decode_json::<QrResponse>(response)
            .await
            .map_err(WeChatControlError::Upstream)?;
        if qr.qrcode.is_empty()
            || qr.qrcode_img_content.is_empty()
            || qr.qrcode.len() > MAX_QR_FIELD_BYTES
            || qr.qrcode_img_content.len() > MAX_QR_FIELD_BYTES
        {
            return Err(WeChatControlError::Upstream(ProviderError::InvalidResponse));
        }

        let id = random_hex(16).map_err(WeChatControlError::Upstream)?;
        *self.inner.login.lock().await = Some(LoginSession {
            id: id.clone(),
            qrcode: qr.qrcode,
            base_url: self.inner.default_base_url.clone(),
            created_at: Instant::now(),
        });

        Ok(StartLoginResponse {
            login_id: id,
            qr_content: qr.qrcode_img_content,
            expires_in_seconds: LOGIN_TTL.as_secs(),
            status: LoginStatus::WaitingForScan,
        })
    }

    pub async fn poll_login(
        &self,
        login_id: &str,
        verification_code: Option<&str>,
    ) -> Result<PollLoginResponse, WeChatControlError> {
        if let Some(code) = verification_code
            && (code.is_empty()
                || code.len() > 12
                || !code.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return Err(WeChatControlError::InvalidVerificationCode);
        }

        let mut login_guard = self.inner.login.lock().await;
        let login = login_guard
            .as_mut()
            .filter(|login| login.id == login_id)
            .ok_or(WeChatControlError::LoginNotFound)?;
        if login.created_at.elapsed() >= LOGIN_TTL {
            *login_guard = None;
            return Err(WeChatControlError::LoginExpired);
        }

        let mut url = login
            .base_url
            .join("ilink/bot/get_qrcode_status")
            .map_err(|_| WeChatControlError::Upstream(ProviderError::InvalidResponse))?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("qrcode", &login.qrcode);
            if let Some(code) = verification_code {
                query.append_pair("verify_code", code);
            }
        }

        let request = self
            .inner
            .client
            .get(url)
            .header("iLink-App-Id", ILINK_APP_ID)
            .header("iLink-App-ClientVersion", ILINK_APP_CLIENT_VERSION)
            .timeout(QR_POLL_TIMEOUT);
        let response = match request.send().await {
            Ok(response) => response,
            Err(error) if error.is_timeout() => {
                return Ok(waiting_for_scan());
            }
            Err(error) => {
                return Err(WeChatControlError::Upstream(ProviderError::Transport(
                    error,
                )));
            }
        };
        let status = decode_json::<QrStatusResponse>(response)
            .await
            .map_err(WeChatControlError::Upstream)?;

        match status.status.as_str() {
            "wait" => Ok(waiting_for_scan()),
            "scaned" => Ok(PollLoginResponse {
                status: LoginStatus::Scanned,
                linked: false,
                ready: false,
                message: "QR code scanned; confirm the connection in WeChat",
            }),
            "need_verifycode" => Ok(PollLoginResponse {
                status: LoginStatus::VerificationRequired,
                linked: false,
                ready: false,
                message: "enter the numeric verification code shown in WeChat",
            }),
            "verify_code_blocked" => {
                *login_guard = None;
                Ok(PollLoginResponse {
                    status: LoginStatus::VerificationBlocked,
                    linked: false,
                    ready: false,
                    message: "verification was blocked; start a new login later",
                })
            }
            "expired" => {
                *login_guard = None;
                Ok(PollLoginResponse {
                    status: LoginStatus::Expired,
                    linked: false,
                    ready: false,
                    message: "QR code expired; start a new login",
                })
            }
            "scaned_but_redirect" => {
                let redirect = status
                    .redirect_host
                    .as_deref()
                    .and_then(trusted_redirect_url)
                    .ok_or(WeChatControlError::UnsafeRedirect)?;
                login.base_url = redirect;
                Ok(PollLoginResponse {
                    status: LoginStatus::Scanned,
                    linked: false,
                    ready: false,
                    message: "QR code scanned; continuing login through WeChat",
                })
            }
            "binded_redirect" => {
                let linked = self.inner.state.read().await.linked();
                if !linked {
                    return Err(WeChatControlError::Upstream(ProviderError::InvalidResponse));
                }
                *login_guard = None;
                drop(login_guard);
                self.restart_monitor().await;
                let ready = self.inner.state.read().await.ready();
                Ok(PollLoginResponse {
                    status: LoginStatus::AlreadyLinked,
                    linked: true,
                    ready,
                    message: "this WeChat ClawBot is already linked",
                })
            }
            "confirmed" => {
                let bot_token = required_response_field(status.bot_token)
                    .map_err(WeChatControlError::Upstream)?;
                let bot_id = required_response_field(status.ilink_bot_id)
                    .map_err(WeChatControlError::Upstream)?;
                let user_id = required_response_field(status.ilink_user_id)
                    .map_err(WeChatControlError::Upstream)?;
                let base_url = match status.baseurl.as_deref() {
                    Some(value) => {
                        let parsed =
                            Url::parse(value).map_err(|_| WeChatControlError::UnsafeRedirect)?;
                        if !self.inner.allow_insecure_base_url && !is_trusted_ilink_url(&parsed) {
                            return Err(WeChatControlError::UnsafeRedirect);
                        }
                        parsed
                    }
                    None => login.base_url.clone(),
                };
                *login_guard = None;
                drop(login_guard);

                let next_state = PersistedState {
                    version: state_version(),
                    bot_token: Some(bot_token),
                    bot_id: Some(bot_id),
                    user_id: Some(user_id),
                    base_url: Some(base_url.to_string()),
                    get_updates_buf: String::new(),
                    context_token: None,
                };
                persist_state(&self.inner.state_path, &next_state)
                    .map_err(WeChatControlError::Persistence)?;
                *self.inner.state.write().await = next_state;
                self.inner.auth_stale.store(false, Ordering::Release);
                self.restart_monitor().await;

                Ok(PollLoginResponse {
                    status: LoginStatus::Linked,
                    linked: true,
                    ready: false,
                    message: "linked; send any message to the new ClawBot in WeChat to activate notifications",
                })
            }
            _ => Err(WeChatControlError::Upstream(ProviderError::InvalidResponse)),
        }
    }

    async fn restart_monitor(&self) {
        let snapshot = self.inner.state.read().await.clone();
        let mut cancel_guard = self.inner.monitor_cancel.lock().await;
        if let Some(cancel) = cancel_guard.take() {
            cancel.cancel();
        }

        let generation = self.inner.monitor_generation.fetch_add(1, Ordering::AcqRel) + 1;
        if !snapshot.linked() {
            self.inner.monitor_running.store(false, Ordering::Release);
            return;
        }

        let cancel = self.inner.shutdown.child_token();
        *cancel_guard = Some(cancel.clone());
        self.inner.monitor_running.store(true, Ordering::Release);
        drop(cancel_guard);

        let provider = self.clone();
        tokio::spawn(async move {
            provider.monitor_loop(snapshot, cancel).await;
            if provider.inner.monitor_generation.load(Ordering::Acquire) == generation {
                provider
                    .inner
                    .monitor_running
                    .store(false, Ordering::Release);
            }
        });
    }

    async fn monitor_loop(&self, snapshot: PersistedState, cancel: CancellationToken) {
        let Some(bot_token) = snapshot.bot_token else {
            return;
        };
        let Some(bot_id) = snapshot.bot_id else {
            return;
        };
        let Some(user_id) = snapshot.user_id else {
            return;
        };
        let Some(base_url) = snapshot
            .base_url
            .as_deref()
            .and_then(|value| Url::parse(value).ok())
        else {
            warn!("stored iLink base URL is invalid; monitor stopped");
            return;
        };

        if let Err(error) = self
            .notify_lifecycle(&base_url, &bot_token, "ilink/bot/msg/notifystart")
            .await
        {
            warn!(reason = %error, "iLink start notification failed");
        }
        info!("WeChat iLink monitor started");
        let mut cursor = snapshot.get_updates_buf;
        let mut timeout_ms = 35_000_u64;
        let mut failures = 0_u8;

        loop {
            if cancel.is_cancelled() {
                break;
            }
            let url = match base_url.join("ilink/bot/getupdates") {
                Ok(url) => url,
                Err(_) => break,
            };
            let request = match self.post_builder(
                url,
                Some(&bot_token),
                Duration::from_millis(timeout_ms + 10_000),
            ) {
                Ok(builder) => builder.json(&json!({
                    "get_updates_buf": &cursor,
                    "base_info": base_info(),
                })),
                Err(error) => {
                    warn!(reason = %error, "failed to build iLink monitor request");
                    break;
                }
            };

            let response = tokio::select! {
                _ = cancel.cancelled() => break,
                result = request.send() => result,
            };
            let response = match response {
                Ok(response) => response,
                Err(error) if error.is_timeout() => {
                    failures = 0;
                    continue;
                }
                Err(_) => {
                    failures = failures.saturating_add(1);
                    warn!(failures, "iLink monitor transport failure");
                    if !sleep_with_cancel(backoff_for(failures), &cancel).await {
                        break;
                    }
                    continue;
                }
            };
            let updates = match decode_json::<UpdatesResponse>(response).await {
                Ok(updates) => updates,
                Err(_) => {
                    failures = failures.saturating_add(1);
                    warn!(failures, "iLink monitor returned an invalid response");
                    if !sleep_with_cancel(backoff_for(failures), &cancel).await {
                        break;
                    }
                    continue;
                }
            };

            let code = nonzero_code(updates.ret, updates.errcode);
            if let Some(code) = code {
                failures = failures.saturating_add(1);
                if code == STALE_TOKEN_CODE {
                    self.inner.auth_stale.store(true, Ordering::Release);
                    warn!("WeChat iLink token is stale; relink is required");
                } else {
                    warn!(code, failures, "iLink monitor request was rejected");
                }
                if !sleep_with_cancel(backoff_for(failures), &cancel).await {
                    break;
                }
                continue;
            }

            failures = 0;
            self.inner.auth_stale.store(false, Ordering::Release);
            if let Some(next_timeout) = updates.longpolling_timeout_ms {
                timeout_ms = next_timeout.clamp(1_000, 60_000);
            }

            let mut next_cursor = cursor.clone();
            if let Some(value) = updates.get_updates_buf.filter(|value| !value.is_empty()) {
                next_cursor = value;
            }
            let next_context = updates
                .msgs
                .iter()
                .rev()
                .find(|message| message.from_user_id.as_deref() == Some(user_id.as_str()))
                .and_then(|message| message.context_token.as_ref())
                .filter(|token| !token.is_empty())
                .cloned();

            if next_cursor != cursor || next_context.is_some() {
                let mut state_guard = self.inner.state.write().await;
                if state_guard.bot_id.as_deref() != Some(bot_id.as_str()) {
                    break;
                }
                let mut next_state = state_guard.clone();
                next_state.get_updates_buf.clone_from(&next_cursor);
                if let Some(context) = next_context {
                    next_state.context_token = Some(context);
                }
                match persist_state(&self.inner.state_path, &next_state) {
                    Ok(()) => {
                        cursor = next_cursor;
                        let became_ready = !state_guard.ready() && next_state.ready();
                        *state_guard = next_state;
                        if became_ready {
                            info!("WeChat ClawBot notification context is ready");
                        }
                    }
                    Err(_) => warn!("failed to persist iLink monitor state"),
                }
            }
        }

        if let Err(error) = self
            .notify_lifecycle(&base_url, &bot_token, "ilink/bot/msg/notifystop")
            .await
        {
            warn!(reason = %error, "iLink stop notification failed");
        }
    }

    async fn notify_lifecycle(
        &self,
        base_url: &Url,
        token: &str,
        endpoint: &str,
    ) -> Result<(), ProviderError> {
        let url = base_url
            .join(endpoint)
            .map_err(|_| ProviderError::InvalidResponse)?;
        let response = self
            .post_json(
                url,
                Some(token),
                &json!({ "base_info": base_info() }),
                Duration::from_secs(10),
            )
            .await?;
        let response = decode_json::<SendResponse>(response).await?;
        if let Some(code) = nonzero_code(response.ret, response.errcode) {
            return Err(ProviderError::Rejected { code });
        }
        Ok(())
    }

    fn post_builder(
        &self,
        url: Url,
        token: Option<&str>,
        timeout: Duration,
    ) -> Result<RequestBuilder, ProviderError> {
        let mut builder = self
            .inner
            .client
            .post(url)
            .header("AuthorizationType", "ilink_bot_token")
            .header("X-WECHAT-UIN", random_wechat_uin()?)
            .header("iLink-App-Id", ILINK_APP_ID)
            .header("iLink-App-ClientVersion", ILINK_APP_CLIENT_VERSION)
            .timeout(timeout);
        if let Some(token) = token {
            builder = builder.bearer_auth(token);
        }
        Ok(builder)
    }

    async fn post_json<T: Serialize + ?Sized>(
        &self,
        url: Url,
        token: Option<&str>,
        value: &T,
        timeout: Duration,
    ) -> Result<Response, ProviderError> {
        self.post_builder(url, token, timeout)?
            .json(value)
            .send()
            .await
            .map_err(ProviderError::Transport)
    }
}

#[async_trait]
impl NotificationProvider for IlinkProvider {
    fn name(&self) -> &'static str {
        "wechat_ilink"
    }

    async fn send(&self, notification: &Notification) -> Result<(), ProviderError> {
        if self.inner.auth_stale.load(Ordering::Acquire) {
            return Err(ProviderError::SessionStale);
        }
        let state = self.inner.state.read().await.clone();
        let bot_token = state.bot_token.ok_or(ProviderError::NotLinked)?;
        let user_id = state.user_id.ok_or(ProviderError::NotLinked)?;
        let context_token = state.context_token.ok_or(ProviderError::ContextNotReady)?;
        let base_url = state
            .base_url
            .as_deref()
            .and_then(|value| Url::parse(value).ok())
            .ok_or(ProviderError::InvalidResponse)?;

        let text = format_notification(notification);
        if text.chars().count() > MAX_TEXT_CHARS {
            return Err(ProviderError::PayloadTooLarge);
        }
        let client_id = format!("we-bot:{}-{}", unix_millis(), random_hex(4)?);
        let request = json!({
            "msg": {
                "from_user_id": "",
                "to_user_id": user_id,
                "client_id": client_id,
                "message_type": 2,
                "message_state": 2,
                "item_list": [{
                    "type": 1,
                    "text_item": { "text": text }
                }],
                "context_token": context_token
            },
            "base_info": base_info()
        });
        let url = base_url
            .join("ilink/bot/sendmessage")
            .map_err(|_| ProviderError::InvalidResponse)?;
        let response = self
            .post_json(url, Some(&bot_token), &request, SEND_TIMEOUT)
            .await?;
        let response = decode_json::<SendResponse>(response).await?;
        if let Some(code) = nonzero_code(response.ret, response.errcode) {
            if code == STALE_TOKEN_CODE {
                self.inner.auth_stale.store(true, Ordering::Release);
                return Err(ProviderError::SessionStale);
            }
            return Err(ProviderError::Rejected { code });
        }
        Ok(())
    }
}

fn base_info() -> serde_json::Value {
    json!({
        "channel_version": ILINK_CHANNEL_VERSION,
        "bot_agent": concat!("we-bot/", env!("CARGO_PKG_VERSION"))
    })
}

fn waiting_for_scan() -> PollLoginResponse {
    PollLoginResponse {
        status: LoginStatus::WaitingForScan,
        linked: false,
        ready: false,
        message: "waiting for the QR code to be scanned",
    }
}

fn required_response_field(value: Option<String>) -> Result<String, ProviderError> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or(ProviderError::InvalidResponse)
}

async fn decode_json<T: for<'de> Deserialize<'de>>(response: Response) -> Result<T, ProviderError> {
    if !response.status().is_success() {
        return Err(ProviderError::Rejected {
            code: i64::from(response.status().as_u16()),
        });
    }
    response
        .json::<T>()
        .await
        .map_err(|_| ProviderError::InvalidResponse)
}

fn nonzero_code(ret: Option<i64>, errcode: Option<i64>) -> Option<i64> {
    ret.filter(|code| *code != 0)
        .or_else(|| errcode.filter(|code| *code != 0))
}

fn format_notification(notification: &Notification) -> String {
    let mut text = format!("{}\n\n{}", notification.title, notification.body);
    if let Some(url) = &notification.url {
        text.push_str("\n\n查看详情：");
        text.push_str(url.as_str());
    }
    text.push_str("\n\n");
    text.push_str(&format!(
        "来源：{} · 事件：{} · 优先级：{}",
        notification.source, notification.event, notification.priority
    ));
    text
}

fn random_wechat_uin() -> Result<String, ProviderError> {
    let mut bytes = [0_u8; 4];
    getrandom::fill(&mut bytes).map_err(|_| ProviderError::Entropy)?;
    let value = u32::from_be_bytes(bytes).to_string();
    Ok(STANDARD.encode(value.as_bytes()))
}

fn random_hex(byte_count: usize) -> Result<String, ProviderError> {
    let mut bytes = vec![0_u8; byte_count];
    getrandom::fill(&mut bytes).map_err(|_| ProviderError::Entropy)?;
    let mut output = String::with_capacity(byte_count * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(output)
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn backoff_for(failures: u8) -> Duration {
    if failures >= 3 {
        Duration::from_secs(30)
    } else {
        Duration::from_secs(2)
    }
}

async fn sleep_with_cancel(duration: Duration, cancel: &CancellationToken) -> bool {
    tokio::select! {
        _ = cancel.cancelled() => false,
        _ = tokio::time::sleep(duration) => true,
    }
}

fn trusted_redirect_url(host: &str) -> Option<Url> {
    if host.is_empty() || host.contains('/') || host.contains('@') {
        return None;
    }
    let url = Url::parse(&format!("https://{host}/")).ok()?;
    is_trusted_ilink_url(&url).then_some(url)
}

fn is_trusted_ilink_url(url: &Url) -> bool {
    if url.scheme() != "https" || url.username() != "" || url.password().is_some() {
        return false;
    }
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        return false;
    }
    if url.port().is_some_and(|port| port != 443) {
        return false;
    }
    url.host_str()
        .is_some_and(|host| host == "weixin.qq.com" || host.ends_with(".weixin.qq.com"))
}

fn load_state(
    path: &Path,
    allow_insecure: bool,
    default_base_url: &Url,
) -> AnyResult<PersistedState> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PersistedState::default());
        }
        Err(error) => return Err(error).context("failed to read WeChat state"),
    };
    let state: PersistedState =
        serde_json::from_slice(&bytes).context("failed to parse WeChat state")?;
    if state.version != state_version() {
        bail!("unsupported WeChat state version");
    }
    let linked_field_count = [
        state.bot_token.is_some(),
        state.bot_id.is_some(),
        state.user_id.is_some(),
        state.base_url.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();
    if linked_field_count != 0 && linked_field_count != 4 {
        bail!("WeChat state is incomplete");
    }
    if state.context_token.is_some() && !state.linked() {
        bail!("WeChat state contains context without an account");
    }
    if let Some(value) = state.base_url.as_deref() {
        let parsed = Url::parse(value).context("stored iLink base URL is invalid")?;
        if !allow_insecure && !is_trusted_ilink_url(&parsed) {
            bail!("stored iLink base URL is not trusted");
        }
    } else if !allow_insecure && !is_trusted_ilink_url(default_base_url) {
        bail!("default iLink base URL is not trusted");
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .context("failed to secure WeChat state permissions")?;
    }
    Ok(state)
}

fn persist_state(path: &Path, state: &PersistedState) -> Result<(), StateWriteError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent_existed = parent.exists();
    fs::create_dir_all(parent).map_err(StateWriteError::Io)?;
    #[cfg(unix)]
    if !parent_existed {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(StateWriteError::Io)?;
    }

    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("state.json");
    let temporary = parent.join(format!(".{file_name}.tmp"));
    let bytes = serde_json::to_vec(state).map_err(StateWriteError::Serialize)?;
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).map_err(StateWriteError::Io)?;
    file.write_all(&bytes).map_err(StateWriteError::Io)?;
    file.sync_all().map_err(StateWriteError::Io)?;
    drop(file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))
            .map_err(StateWriteError::Io)?;
    }
    fs::rename(&temporary, path).map_err(StateWriteError::Io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    use axum::{
        Json, Router,
        extract::State,
        http::{HeaderMap, Uri},
        routing::{get, post},
    };
    use reqwest::Url;
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use tokio::sync::{Mutex, Notify};
    use tokio_util::sync::CancellationToken;

    use super::{IlinkProvider, WeChatConnectionState, format_notification, trusted_redirect_url};
    use crate::{
        model::{Notification, Priority},
        provider::NotificationProvider,
    };

    #[derive(Clone)]
    struct MockApi {
        update_sent: Arc<AtomicBool>,
        send_payload: Arc<Mutex<Option<Value>>>,
        send_headers: Arc<Mutex<Option<HeaderMap>>>,
        qr_request: Arc<Mutex<Option<(HeaderMap, Uri, Value)>>>,
        send_notify: Arc<Notify>,
    }

    async fn qr(
        State(state): State<MockApi>,
        headers: HeaderMap,
        uri: Uri,
        Json(payload): Json<Value>,
    ) -> Json<Value> {
        *state.qr_request.lock().await = Some((headers, uri, payload));
        Json(json!({
            "qrcode": "opaque-qr-id",
            "qrcode_img_content": "https://example.test/qr-content"
        }))
    }

    async fn qr_status() -> Json<Value> {
        Json(json!({
            "status": "confirmed",
            "bot_token": "ilink-secret-token",
            "ilink_bot_id": "bot-id@im.bot",
            "ilink_user_id": "owner-user-id"
        }))
    }

    async fn updates(State(state): State<MockApi>) -> Json<Value> {
        if !state.update_sent.swap(true, Ordering::AcqRel) {
            return Json(json!({
                "ret": 0,
                "get_updates_buf": "next-cursor",
                "longpolling_timeout_ms": 1000,
                "msgs": [{
                    "from_user_id": "owner-user-id",
                    "context_token": "owner-context-token"
                }, {
                    "from_user_id": "untrusted-user-id",
                    "context_token": "untrusted-context-token"
                }]
            }));
        }
        tokio::time::sleep(Duration::from_secs(60)).await;
        Json(json!({ "ret": 0, "msgs": [] }))
    }

    async fn send_message(
        State(state): State<MockApi>,
        headers: HeaderMap,
        Json(payload): Json<Value>,
    ) -> Json<Value> {
        *state.send_payload.lock().await = Some(payload);
        *state.send_headers.lock().await = Some(headers);
        state.send_notify.notify_one();
        Json(json!({ "ret": 0 }))
    }

    async fn lifecycle() -> Json<Value> {
        Json(json!({ "ret": 0 }))
    }

    async fn spawn_api() -> (MockApi, Url, tokio::task::JoinHandle<()>) {
        let state = MockApi {
            update_sent: Arc::new(AtomicBool::new(false)),
            send_payload: Arc::new(Mutex::new(None)),
            send_headers: Arc::new(Mutex::new(None)),
            qr_request: Arc::new(Mutex::new(None)),
            send_notify: Arc::new(Notify::new()),
        };
        let app = Router::new()
            .route("/ilink/bot/get_bot_qrcode", post(qr))
            .route("/ilink/bot/get_qrcode_status", get(qr_status))
            .route("/ilink/bot/getupdates", post(updates))
            .route("/ilink/bot/sendmessage", post(send_message))
            .route("/ilink/bot/msg/notifystart", post(lifecycle))
            .route("/ilink/bot/msg/notifystop", post(lifecycle))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (state, url, handle)
    }

    fn notification() -> Notification {
        Notification {
            source: "codex".to_owned(),
            event: "task.completed".to_owned(),
            title: "任务完成".to_owned(),
            body: "构建通过".to_owned(),
            url: Some(Url::parse("https://example.com/task").unwrap()),
            priority: Priority::High,
            dedupe_key: Some("private-id".to_owned()),
        }
    }

    #[test]
    fn formats_plain_text_without_dedupe_key() {
        let content = format_notification(&notification());
        assert!(content.starts_with("任务完成\n\n构建通过"));
        assert!(content.contains("查看详情：https://example.com/task"));
        assert!(content.contains("优先级：high"));
        assert!(!content.contains("private-id"));
        assert!(!content.contains("# 任务完成"));
    }

    #[test]
    fn redirect_hosts_are_restricted_to_weixin() {
        assert!(trusted_redirect_url("ilinkai.weixin.qq.com").is_some());
        assert!(trusted_redirect_url("region.weixin.qq.com").is_some());
        assert!(trusted_redirect_url("weixin.qq.com.attacker.example").is_none());
        assert!(trusted_redirect_url("user@weixin.qq.com").is_none());
        assert!(trusted_redirect_url("weixin.qq.com/path").is_none());
    }

    #[tokio::test]
    async fn login_monitor_and_send_use_ilink_protocol() {
        let (api, base_url, server) = spawn_api().await;
        let directory = TempDir::new().unwrap();
        let state_path = directory.path().join("state.json");
        let shutdown = CancellationToken::new();
        let provider =
            IlinkProvider::new_for_test(base_url, state_path.clone(), shutdown.clone()).unwrap();

        let login = provider.start_login().await.unwrap();
        assert_eq!(login.qr_content, "https://example.test/qr-content");
        let qr_request = api.qr_request.lock().await.clone().unwrap();
        assert_eq!(qr_request.0["authorizationtype"], "ilink_bot_token");
        assert_eq!(qr_request.0["ilink-app-id"], "bot");
        assert_eq!(qr_request.0["ilink-app-clientversion"], "132104");
        assert_eq!(qr_request.1.query(), Some("bot_type=3"));
        assert_eq!(qr_request.2["local_token_list"], json!([]));
        let linked = provider.poll_login(&login.login_id, None).await.unwrap();
        assert!(linked.linked);
        assert!(!linked.ready);
        let public_login_response = serde_json::to_string(&linked).unwrap();
        assert!(!public_login_response.contains("ilink-secret-token"));
        assert!(!public_login_response.contains("owner-user-id"));

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if matches!(provider.status().await.state, WeChatConnectionState::Ready) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        provider.send(&notification()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), api.send_notify.notified())
            .await
            .unwrap();

        let payload = api.send_payload.lock().await.clone().unwrap();
        assert_eq!(payload["msg"]["to_user_id"], "owner-user-id");
        assert_eq!(payload["msg"]["context_token"], "owner-context-token");
        assert_eq!(payload["msg"]["message_type"], 2);
        assert_eq!(payload["msg"]["message_state"], 2);
        assert_eq!(payload["base_info"]["channel_version"], "2.4.8");
        assert_eq!(payload["base_info"]["bot_agent"], "we-bot/0.2.0");

        let headers = api.send_headers.lock().await.clone().unwrap();
        assert_eq!(headers["authorizationtype"], "ilink_bot_token");
        assert_eq!(headers["ilink-app-id"], "bot");
        assert_eq!(headers["authorization"], "Bearer ilink-secret-token");
        assert!(!headers["x-wechat-uin"].is_empty());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&state_path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let stored: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
        assert_eq!(stored["get_updates_buf"], "next-cursor");
        assert_eq!(stored["context_token"], "owner-context-token");

        shutdown.cancel();
        server.abort();
    }

    #[tokio::test]
    async fn verification_code_must_be_numeric() {
        let directory = TempDir::new().unwrap();
        let provider = IlinkProvider::new_for_test(
            Url::parse("http://127.0.0.1:9/").unwrap(),
            directory.path().join("state.json"),
            CancellationToken::new(),
        )
        .unwrap();
        let error = provider
            .poll_login("missing", Some("12ab"))
            .await
            .unwrap_err();
        assert!(error.is_validation());
    }
}
