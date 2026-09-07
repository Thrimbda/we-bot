use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{config::Secret, model::Notification};

const WXPUSHER_SIMPLE_PUSH_URL: &str = "https://wxpusher.zjiecode.com/api/send/message/simple-push";

#[async_trait]
pub trait NotificationProvider: Send + Sync {
    fn name(&self) -> &'static str;
    async fn send(&self, notification: &Notification) -> Result<(), ProviderError>;
}

#[derive(Clone)]
pub struct WxPusherProvider {
    client: reqwest::Client,
    spt: Secret,
    endpoint: String,
}

impl WxPusherProvider {
    pub fn new(spt: Secret) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent(concat!("we-bot/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            client,
            spt,
            endpoint: WXPUSHER_SIMPLE_PUSH_URL.to_owned(),
        })
    }

    #[cfg(test)]
    pub fn with_endpoint(spt: Secret, endpoint: String) -> Result<Self, reqwest::Error> {
        let mut provider = Self::new(spt)?;
        provider.endpoint = endpoint;
        Ok(provider)
    }
}

#[async_trait]
impl NotificationProvider for WxPusherProvider {
    fn name(&self) -> &'static str {
        "wxpusher"
    }

    async fn send(&self, notification: &Notification) -> Result<(), ProviderError> {
        let content = format_content(notification);
        if content.chars().count() > 40_000 || content.len() > 65_535 {
            return Err(ProviderError::PayloadTooLarge);
        }

        let response = self
            .client
            .post(&self.endpoint)
            .json(&WxPusherRequest {
                content: &content,
                summary: &notification.title,
                content_type: 3,
                spt: self.spt.expose(),
                url: notification.url.as_ref().map(url::Url::as_str),
            })
            .send()
            .await
            .map_err(ProviderError::Transport)?;

        let status = response.status();
        let body = response
            .json::<WxPusherResponse>()
            .await
            .map_err(ProviderError::InvalidResponse)?;

        if !status.is_success() || body.code != 1000 || !body.success {
            return Err(ProviderError::Rejected {
                code: body.code,
                message: sanitize_message(body.message),
            });
        }

        let _ = body.data;
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WxPusherRequest<'a> {
    content: &'a str,
    summary: &'a str,
    content_type: u8,
    spt: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<&'a str>,
}

#[derive(Deserialize)]
struct WxPusherResponse {
    code: i64,
    #[serde(default, rename = "msg")]
    message: String,
    #[serde(default)]
    data: Value,
    #[serde(default)]
    success: bool,
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("provider request failed: {0}")]
    Transport(reqwest::Error),
    #[error("provider returned an invalid response: {0}")]
    InvalidResponse(reqwest::Error),
    #[error("provider rejected the request with code {code}: {message}")]
    Rejected { code: i64, message: String },
    #[error("formatted notification exceeds the provider size limit")]
    PayloadTooLarge,
}

fn format_content(notification: &Notification) -> String {
    let mut content = format!("# {}\n\n{}", notification.title, notification.body);
    if let Some(url) = &notification.url {
        content.push_str("\n\n[查看详情](");
        content.push_str(url.as_str());
        content.push(')');
    }
    content.push_str("\n\n---\n");
    content.push_str(&format!(
        "来源：{} · 事件：{} · 优先级：{}",
        notification.source, notification.event, notification.priority
    ));
    content
}

fn sanitize_message(message: String) -> String {
    if message.contains("SPT_") {
        return "provider rejected the request".to_owned();
    }
    message.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use axum::{Json, Router, extract::State, routing::post};
    use serde_json::{Value, json};
    use tokio::sync::mpsc;

    use super::{NotificationProvider, WxPusherProvider, format_content};
    use crate::config::Secret;
    use crate::model::{Notification, Priority};

    async fn fake_wxpusher(
        State(sender): State<mpsc::UnboundedSender<Value>>,
        Json(payload): Json<Value>,
    ) -> Json<Value> {
        sender.send(payload).unwrap();
        Json(json!({
            "code": 1000,
            "msg": "处理成功",
            "data": {"taskId": 42},
            "success": true
        }))
    }

    #[test]
    fn formats_markdown_without_exposing_dedupe_key() {
        let message = Notification {
            source: "codex".to_owned(),
            event: "task.completed".to_owned(),
            title: "任务完成".to_owned(),
            body: "构建通过".to_owned(),
            url: Some(url::Url::parse("https://example.com/task").unwrap()),
            priority: Priority::High,
            dedupe_key: Some("private-id".to_owned()),
        };
        let content = format_content(&message);
        assert!(content.contains("# 任务完成"));
        assert!(content.contains("[查看详情](https://example.com/task)"));
        assert!(content.contains("优先级：high"));
        assert!(!content.contains("private-id"));
    }

    #[tokio::test]
    async fn sends_the_documented_simple_push_shape() {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let app = Router::new()
            .route("/simple-push", post(fake_wxpusher))
            .with_state(sender);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/simple-push", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let provider =
            WxPusherProvider::with_endpoint(Secret::new("SPT_test-only".to_owned()), endpoint)
                .unwrap();
        provider
            .send(&Notification {
                source: "codex".to_owned(),
                event: "task.completed".to_owned(),
                title: "任务完成".to_owned(),
                body: "构建通过".to_owned(),
                url: None,
                priority: Priority::Normal,
                dedupe_key: None,
            })
            .await
            .unwrap();

        let payload = receiver.recv().await.unwrap();
        assert_eq!(payload["summary"], "任务完成");
        assert_eq!(payload["contentType"], 3);
        assert_eq!(payload["spt"], "SPT_test-only");
        assert!(payload["content"].as_str().unwrap().contains("构建通过"));

        server.abort();
    }
}
