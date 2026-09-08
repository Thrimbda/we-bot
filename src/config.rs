use std::{
    env, fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail};

#[derive(Clone)]
pub struct Secret(Arc<str>);

impl Secret {
    pub fn new(value: String) -> Self {
        Self(Arc::from(value))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

#[derive(Clone)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub api_token: Secret,
    pub state_path: PathBuf,
    pub allowed_hosts: Vec<String>,
    pub dedupe_ttl: Duration,
    pub rate_limit_per_minute: usize,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let bind_addr = env::var("WE_BOT_BIND_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:3099".to_owned())
            .parse()
            .context("WE_BOT_BIND_ADDR must be a socket address")?;

        let api_token = read_secret("WE_BOT_API_TOKEN", "WE_BOT_API_TOKEN_FILE")?;
        if !(32..=512).contains(&api_token.len()) {
            bail!("WE_BOT_API_TOKEN must contain between 32 and 512 bytes");
        }

        let state_path = env::var_os("WE_BOT_STATE_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("./data/state.json"));
        if state_path.file_name().is_none() {
            bail!("WE_BOT_STATE_PATH must point to a file");
        }

        let allowed_hosts = env::var("WE_BOT_ALLOWED_HOSTS")
            .unwrap_or_else(|_| "localhost,127.0.0.1,::1".to_owned())
            .split(',')
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        if allowed_hosts.is_empty() {
            bail!("WE_BOT_ALLOWED_HOSTS must contain at least one host");
        }

        let dedupe_ttl = Duration::from_secs(parse_number(
            "WE_BOT_DEDUPE_TTL_SECONDS",
            600_u64,
            1,
            86_400,
        )?);
        let rate_limit_per_minute =
            parse_number("WE_BOT_RATE_LIMIT_PER_MINUTE", 60_usize, 1, 10_000)?;

        Ok(Self {
            bind_addr,
            api_token: Secret::new(api_token),
            state_path,
            allowed_hosts,
            dedupe_ttl,
            rate_limit_per_minute,
        })
    }
}

fn read_secret(value_name: &str, file_name: &str) -> Result<String> {
    let value = match (env::var_os(value_name), env::var_os(file_name)) {
        (Some(value), None) => value
            .into_string()
            .map_err(|_| anyhow::anyhow!("{value_name} is not valid UTF-8"))?,
        (None, Some(path)) => {
            let path = Path::new(&path);
            fs::read_to_string(path).with_context(|| format!("failed to read {file_name}"))?
        }
        (Some(_), Some(_)) => {
            bail!("set only one of {value_name} and {file_name}");
        }
        (None, None) => {
            bail!("set either {value_name} or {file_name}");
        }
    };

    let value = value.trim().to_owned();
    if value.is_empty() {
        bail!("{value_name} must not be empty");
    }
    Ok(value)
}

fn parse_number<T>(name: &str, default: T, min: T, max: T) -> Result<T>
where
    T: Copy + PartialOrd + std::str::FromStr + std::fmt::Display,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let value = match env::var(name) {
        Ok(raw) => raw
            .parse::<T>()
            .with_context(|| format!("{name} must be a number"))?,
        Err(env::VarError::NotPresent) => default,
        Err(error) => return Err(error).with_context(|| format!("failed to read {name}")),
    };

    if value < min || value > max {
        bail!("{name} must be between {min} and {max}");
    }
    Ok(value)
}
