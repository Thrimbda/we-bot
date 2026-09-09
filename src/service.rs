use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

use thiserror::Error;
use tokio::sync::Mutex;

use crate::{
    model::{DeliveryStatus, Notification, NotificationInput, NotifyResponse},
    provider::{NotificationProvider, ProviderError},
};

#[derive(Clone)]
pub struct NotificationService {
    provider: Arc<dyn NotificationProvider>,
    dedupe: Arc<Mutex<HashMap<String, Instant>>>,
    rate: Arc<Mutex<VecDeque<Instant>>>,
    dedupe_ttl: Duration,
    rate_limit_per_minute: usize,
}

impl NotificationService {
    pub fn new(
        provider: Arc<dyn NotificationProvider>,
        dedupe_ttl: Duration,
        rate_limit_per_minute: usize,
    ) -> Self {
        Self {
            provider,
            dedupe: Arc::new(Mutex::new(HashMap::new())),
            rate: Arc::new(Mutex::new(VecDeque::new())),
            dedupe_ttl,
            rate_limit_per_minute,
        }
    }

    pub fn prepare(
        &self,
        input: NotificationInput,
        default_source: &str,
    ) -> Result<Notification, NotifyError> {
        input
            .validate(default_source)
            .map_err(NotifyError::Validation)
    }

    pub async fn notify(
        &self,
        input: NotificationInput,
        default_source: &str,
    ) -> Result<NotifyResponse, NotifyError> {
        let notification = self.prepare(input, default_source)?;
        self.deliver(notification).await
    }

    pub async fn deliver(&self, notification: Notification) -> Result<NotifyResponse, NotifyError> {
        let reserved_key = self.reserve_dedupe_key(&notification).await;
        if notification.dedupe_key.is_some() && reserved_key.is_none() {
            return Ok(NotifyResponse {
                status: DeliveryStatus::Duplicate,
                provider: None,
            });
        }

        if let Err(error) = self.acquire_rate_slot().await {
            self.release_dedupe_key(reserved_key.as_deref()).await;
            return Err(error);
        }

        if let Err(error) = self.provider.send(&notification).await {
            self.release_dedupe_key(reserved_key.as_deref()).await;
            return Err(NotifyError::Provider(error));
        }

        Ok(NotifyResponse {
            status: DeliveryStatus::Sent,
            provider: Some(self.provider.name().to_owned()),
        })
    }

    async fn reserve_dedupe_key(&self, notification: &Notification) -> Option<String> {
        let key = notification.dedupe_key.as_ref()?;

        let now = Instant::now();
        let mut entries = self.dedupe.lock().await;
        entries.retain(|_, created| now.duration_since(*created) < self.dedupe_ttl);
        if entries.contains_key(key) {
            return None;
        }
        entries.insert(key.clone(), now);
        Some(key.clone())
    }

    async fn release_dedupe_key(&self, key: Option<&str>) {
        if let Some(key) = key {
            self.dedupe.lock().await.remove(key);
        }
    }

    pub(crate) async fn acquire_rate_slot(&self) -> Result<(), NotifyError> {
        let now = Instant::now();
        let window = Duration::from_secs(60);
        let mut entries = self.rate.lock().await;
        while entries
            .front()
            .is_some_and(|created| now.duration_since(*created) >= window)
        {
            entries.pop_front();
        }

        if entries.len() >= self.rate_limit_per_minute {
            let retry_after = entries
                .front()
                .map(|created| {
                    window
                        .saturating_sub(now.duration_since(*created))
                        .as_secs()
                        + 1
                })
                .unwrap_or(1);
            return Err(NotifyError::RateLimited { retry_after });
        }

        entries.push_back(now);
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum NotifyError {
    #[error("invalid notification: {0}")]
    Validation(String),
    #[error("notification rate limit exceeded")]
    RateLimited { retry_after: u64 },
    #[error(transparent)]
    Provider(ProviderError),
}

impl NotifyError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Validation(_) => "invalid_notification",
            Self::RateLimited { .. } => "rate_limited",
            Self::Provider(error) => error.code(),
        }
    }

    pub fn public_message(&self) -> String {
        match self {
            Self::Validation(message) => message.clone(),
            Self::RateLimited { .. } => "notification rate limit exceeded".to_owned(),
            Self::Provider(error) => error.public_message().to_owned(),
        }
    }

    pub fn retry_after(&self) -> Option<u64> {
        match self {
            Self::RateLimited { retry_after } => Some(*retry_after),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use async_trait::async_trait;

    use super::{NotificationService, NotifyError};
    use crate::{
        model::{DeliveryStatus, Notification, NotificationInput, Priority},
        provider::{NotificationProvider, ProviderError},
    };

    struct MockProvider {
        calls: AtomicUsize,
        fail: AtomicBool,
    }

    #[async_trait]
    impl NotificationProvider for MockProvider {
        fn name(&self) -> &'static str {
            "mock"
        }

        async fn send(&self, _notification: &Notification) -> Result<(), ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                return Err(ProviderError::PayloadTooLarge);
            }
            Ok(())
        }
    }

    fn input(dedupe_key: Option<&str>) -> NotificationInput {
        NotificationInput {
            source: Some("test".to_owned()),
            event: Some("test.sent".to_owned()),
            title: "Test".to_owned(),
            body: "Body".to_owned(),
            url: None,
            priority: Priority::Normal,
            dedupe_key: dedupe_key.map(ToOwned::to_owned),
        }
    }

    #[tokio::test]
    async fn suppresses_duplicate_keys() {
        let provider = Arc::new(MockProvider {
            calls: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
        });
        let service = NotificationService::new(provider.clone(), Duration::from_secs(60), 10);

        let first = service.notify(input(Some("same")), "test").await.unwrap();
        let second = service.notify(input(Some("same")), "test").await.unwrap();

        assert!(matches!(first.status, DeliveryStatus::Sent));
        assert!(matches!(second.status, DeliveryStatus::Duplicate));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failed_delivery_does_not_poison_dedupe_key() {
        let provider = Arc::new(MockProvider {
            calls: AtomicUsize::new(0),
            fail: AtomicBool::new(true),
        });
        let service = NotificationService::new(provider.clone(), Duration::from_secs(60), 10);

        assert!(service.notify(input(Some("retry")), "test").await.is_err());
        provider.fail.store(false, Ordering::SeqCst);
        let retried = service.notify(input(Some("retry")), "test").await.unwrap();

        assert!(matches!(retried.status, DeliveryStatus::Sent));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn enforces_global_rate_limit() {
        let provider = Arc::new(MockProvider {
            calls: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
        });
        let service = NotificationService::new(provider, Duration::from_secs(60), 1);

        service.notify(input(None), "test").await.unwrap();
        let error = service.notify(input(None), "test").await.unwrap_err();

        assert!(matches!(error, NotifyError::RateLimited { .. }));
    }
}
