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
    #[error("WeChat iLink rejected message preparation; a new owner message is required")]
    SendBlocked,
    #[error("WeChat iLink session is stale and must be linked again")]
    SessionStale,
    #[error("iLink request failed")]
    Transport(#[source] reqwest::Error),
    #[error("iLink returned an invalid response")]
    InvalidResponse,
    #[error("iLink returned HTTP {code} without a delivery acknowledgement")]
    HttpStatus { code: u16 },
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
            Self::SendBlocked => "wechat_send_blocked",
            Self::SessionStale => "wechat_relink_required",
            Self::PayloadTooLarge => "notification_too_large",
            Self::Rejected { .. } => "wechat_request_rejected",
            Self::Transport(_)
            | Self::InvalidResponse
            | Self::HttpStatus { .. }
            | Self::Entropy => "provider_unavailable",
        }
    }

    pub fn public_message(&self) -> &'static str {
        match self {
            Self::AccountNotFound => {
                "the selected WeChat account is no longer bound to this server"
            }
            Self::NotLinked => "WeChat ClawBot has not been linked",
            Self::ContextNotReady => {
                "send a new message to the linked WeChat ClawBot to refresh the sending context"
            }
            Self::SendBlocked => {
                "WeChat rejected this message (prepare failed); send a new message to ClawBot in WeChat before retrying"
            }
            Self::SessionStale => "the WeChat ClawBot session expired; link it again",
            Self::PayloadTooLarge => "formatted notification exceeds 4000 characters",
            Self::Rejected { .. } => "WeChat iLink explicitly rejected the request",
            Self::Transport(_)
            | Self::InvalidResponse
            | Self::HttpStatus { .. }
            | Self::Entropy => "the WeChat iLink sending result could not be confirmed",
        }
    }

    pub fn is_setup_error(&self) -> bool {
        matches!(
            self,
            Self::NotLinked | Self::ContextNotReady | Self::SendBlocked | Self::SessionStale
        )
    }
}
