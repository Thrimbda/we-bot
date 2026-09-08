use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NotificationInput {
    /// Agent or system that emitted the notification.
    pub source: Option<String>,
    /// Machine-readable event name, for example task.completed.
    pub event: Option<String>,
    /// Human-readable notification title.
    pub title: String,
    /// Plain-text notification body.
    pub body: String,
    /// Optional HTTP(S) link associated with the notification.
    pub url: Option<String>,
    /// Delivery importance. Defaults to normal.
    #[serde(default)]
    pub priority: Priority,
    /// Caller-defined idempotency key used for in-memory deduplication.
    pub dedupe_key: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    #[default]
    Normal,
    High,
    Critical,
}

impl std::fmt::Display for Priority {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Low => "low",
            Self::Normal => "normal",
            Self::High => "high",
            Self::Critical => "critical",
        };
        formatter.write_str(value)
    }
}

#[derive(Clone, Debug)]
pub struct Notification {
    pub source: String,
    pub event: String,
    pub title: String,
    pub body: String,
    pub url: Option<Url>,
    pub priority: Priority,
    pub dedupe_key: Option<String>,
}

impl NotificationInput {
    pub fn validate(self, default_source: &str) -> Result<Notification, String> {
        let title = required_field("title", self.title, 100)?;
        let body = required_field("body", self.body, 38_000)?;
        if body.len() > 60_000 {
            return Err("body must contain at most 60000 UTF-8 bytes".to_owned());
        }

        let source =
            optional_field("source", self.source, 64)?.unwrap_or_else(|| default_source.to_owned());
        let event =
            optional_field("event", self.event, 128)?.unwrap_or_else(|| "notification".to_owned());
        let dedupe_key = optional_field("dedupe_key", self.dedupe_key, 200)?;

        let url = optional_field("url", self.url, 2_048)?
            .map(|value| {
                let parsed = Url::parse(&value).map_err(|_| "url must be a valid URL")?;
                if !matches!(parsed.scheme(), "http" | "https") {
                    return Err("url must use http or https");
                }
                Ok(parsed)
            })
            .transpose()
            .map_err(str::to_owned)?;

        Ok(Notification {
            source,
            event,
            title,
            body,
            url,
            priority: self.priority,
            dedupe_key,
        })
    }
}

fn required_field(name: &str, value: String, max_chars: usize) -> Result<String, String> {
    let value = value.trim().to_owned();
    if value.is_empty() {
        return Err(format!("{name} must not be empty"));
    }
    if value.chars().count() > max_chars {
        return Err(format!(
            "{name} must contain at most {max_chars} characters"
        ));
    }
    Ok(value)
}

fn optional_field(
    name: &str,
    value: Option<String>,
    max_chars: usize,
) -> Result<Option<String>, String> {
    value
        .map(|value| required_field(name, value, max_chars))
        .transpose()
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    Sent,
    Duplicate,
}

#[derive(Clone, Debug, JsonSchema, Serialize)]
pub struct NotifyResponse {
    pub status: DeliveryStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchNotifyRequest {
    pub notifications: Vec<NotificationInput>,
}

#[derive(Debug, Serialize)]
pub struct BatchNotifyResponse {
    pub results: Vec<BatchItemResponse>,
}

#[derive(Debug, Serialize)]
pub struct BatchItemResponse {
    pub index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<NotifyResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ErrorEnvelope {
    pub error: ErrorBody,
}

#[derive(Clone, Debug, Serialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub provider: &'static str,
}

#[cfg(test)]
mod tests {
    use super::{NotificationInput, Priority};

    fn input() -> NotificationInput {
        NotificationInput {
            source: None,
            event: None,
            title: " Task done ".to_owned(),
            body: " Finished ".to_owned(),
            url: None,
            priority: Priority::Normal,
            dedupe_key: None,
        }
    }

    #[test]
    fn defaults_and_trims_fields() {
        let notification = input().validate("rest").unwrap();
        assert_eq!(notification.source, "rest");
        assert_eq!(notification.event, "notification");
        assert_eq!(notification.title, "Task done");
        assert_eq!(notification.body, "Finished");
    }

    #[test]
    fn rejects_non_http_url() {
        let mut value = input();
        value.url = Some("file:///tmp/secret".to_owned());
        assert_eq!(
            value.validate("rest").unwrap_err(),
            "url must use http or https"
        );
    }
}
