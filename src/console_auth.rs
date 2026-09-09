use std::{net::IpAddr, time::Duration};

use anyhow::{Context, Result, bail};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use reqwest::{Client, Url, redirect::Policy};

/// Browser authentication is delegated to the existing Auth Mini gateway.
/// The gateway verifies its signed cookie, refreshes sessions and enforces its user allowlist.
#[derive(Clone)]
pub struct ConsoleAuth {
    client: Client,
    check_url: Url,
    pub origin: String,
}

pub struct SessionCheck {
    pub status: StatusCode,
    pub cookies: Vec<HeaderValue>,
}

impl ConsoleAuth {
    pub fn new(gateway_url: &str, public_origin: &str) -> Result<Self> {
        let gateway = Url::parse(gateway_url).context("invalid Auth Mini gateway URL")?;
        let origin = Url::parse(public_origin).context("invalid console origin")?;
        let loopback = gateway.host_str().is_some_and(|host| {
            host.trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
        });
        if gateway.scheme() != "http" || !loopback || !is_origin(&gateway) {
            bail!("WE_BOT_AUTH_GATEWAY_URL must be an HTTP loopback origin");
        }
        if origin.scheme() != "https" || !is_origin(&origin) {
            bail!("WE_BOT_CONSOLE_ORIGIN must be an HTTPS origin");
        }
        Ok(Self {
            client: Client::builder()
                .redirect(Policy::none())
                .timeout(Duration::from_secs(10))
                .build()?,
            check_url: gateway.join("/auth/check")?,
            origin: origin.origin().ascii_serialization(),
        })
    }

    pub async fn check(&self, headers: &HeaderMap) -> SessionCheck {
        let unauthenticated = || SessionCheck {
            status: StatusCode::UNAUTHORIZED,
            cookies: vec![],
        };
        let Some(cookie) = session_cookie(headers) else {
            return unauthenticated();
        };
        // Never forward browser Authorization, spoofable identity headers, or other sites' cookies.
        let response = match self
            .client
            .get(self.check_url.clone())
            .header(header::COOKIE, cookie)
            .send()
            .await
        {
            Ok(response) => response,
            Err(_) => {
                return SessionCheck {
                    status: StatusCode::SERVICE_UNAVAILABLE,
                    cookies: vec![],
                };
            }
        };
        let status = match response.status() {
            StatusCode::NO_CONTENT => StatusCode::NO_CONTENT,
            StatusCode::UNAUTHORIZED => StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN => StatusCode::FORBIDDEN,
            _ => StatusCode::SERVICE_UNAVAILABLE,
        };
        let cookies = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter(|value| {
                value.as_bytes().starts_with(b"amg_session=") && value.as_bytes().len() <= 4096
            })
            .cloned()
            .collect();
        SessionCheck { status, cookies }
    }
}

fn is_origin(url: &Url) -> bool {
    url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none()
}

fn session_cookie(headers: &HeaderMap) -> Option<String> {
    let mut session = None;
    for value in headers.get_all(header::COOKIE) {
        let raw = value.to_str().ok()?;
        if raw.len() > 8192 {
            return None;
        }
        for cookie in raw.split(';') {
            let (name, value) = cookie.trim().split_once('=')?;
            if name == "amg_session" {
                if session.is_some() || value.is_empty() || value.len() > 2048 {
                    return None;
                }
                session = Some(format!("amg_session={value}"));
            }
        }
    }
    session
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_must_be_loopback_and_public_origin_must_be_https() {
        assert!(ConsoleAuth::new("http://127.0.0.1:7783", "https://notify.example.com").is_ok());
        for gateway in [
            "https://remote.example",
            "http://127.0.0.1:7783/path",
            "http://user@127.0.0.1:7783",
            "http://127.0.0.1:7783?query=1",
        ] {
            assert!(ConsoleAuth::new(gateway, "https://notify.example.com").is_err());
        }
        assert!(ConsoleAuth::new("http://127.0.0.1:7783", "http://notify.example.com").is_err());
    }

    #[test]
    fn only_the_unambiguous_gateway_session_cookie_is_forwarded() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("other=private; amg_session=signed; tracking=omit"),
        );
        assert_eq!(
            session_cookie(&headers).as_deref(),
            Some("amg_session=signed")
        );
        headers.append(
            header::COOKIE,
            HeaderValue::from_static("amg_session=second"),
        );
        assert!(session_cookie(&headers).is_none());
    }
}
