use async_trait::async_trait;
use thiserror::Error;

use crate::model::Notification;

#[async_trait]
pub trait NotificationProvider: Send + Sync {
    fn name(&self) -> &'static str;
    async fn send(&self, notification: &Notification) -> Result<(), ProviderError>;
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("the selected WeChat account was not found")]
    AccountNotFound,
    #[error("WeChat ClawBot is not linked")]
    NotLinked,
    #[error("WeChat ClawBot is waiting for the owner to send it a message")]
    ContextNotReady,
    #[error("WeChat iLink session is stale and must be linked again")]
    SessionStale,
    #[error("iLink request failed")]
    Transport(#[source] reqwest::Error),
    #[error("iLink returned an invalid response")]
    InvalidResponse,
    #[error("iLink rejected the request with code {code}")]
    Rejected { code: i64 },
    #[error("formatted notification exceeds the WeChat text limit")]
    PayloadTooLarge,
    #[error("failed to generate request entropy")]
    Entropy,
}

impl ProviderError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::AccountNotFound => "account_not_found",
            Self::NotLinked => "wechat_not_linked",
            Self::ContextNotReady => "wechat_context_not_ready",
            Self::SessionStale => "wechat_relink_required",
            Self::PayloadTooLarge => "notification_too_large",
            Self::Transport(_) | Self::InvalidResponse | Self::Rejected { .. } | Self::Entropy => {
                "provider_unavailable"
            }
        }
    }

    pub fn public_message(&self) -> &'static str {
        match self {
            Self::AccountNotFound => {
                "the selected WeChat account is no longer bound to this server"
            }
            Self::NotLinked => "WeChat ClawBot has not been linked",
            Self::ContextNotReady => {
                "send any message to the linked WeChat ClawBot before sending notifications"
            }
            Self::SessionStale => "the WeChat ClawBot session expired; link it again",
            Self::PayloadTooLarge => "formatted notification exceeds 4000 characters",
            Self::Transport(_) | Self::InvalidResponse | Self::Rejected { .. } | Self::Entropy => {
                "WeChat iLink did not accept the notification"
            }
        }
    }

    pub fn is_setup_error(&self) -> bool {
        matches!(
            self,
            Self::NotLinked | Self::ContextNotReady | Self::SessionStale
        )
    }
}
