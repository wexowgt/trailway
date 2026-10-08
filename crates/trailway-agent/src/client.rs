use std::time::Duration;

use reqwest::StatusCode;
use trailway_proto::{Heartbeat, RegisterRequest, RegisterResponse, UsageAck, UsageSample};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub enum ClientError {
    /// The API rejected our credentials (401).
    Unauthorized,
    Other(anyhow::Error),
}

impl From<reqwest::Error> for ClientError {
    fn from(e: reqwest::Error) -> Self {
        Self::Other(e.into())
    }
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized => write!(f, "unauthorized"),
            Self::Other(e) => write!(f, "{e:#}"),
        }
    }
}

pub struct ApiClient {
    http: reqwest::Client,
    api: String,
}

impl ApiClient {
    pub fn new(api: &str) -> anyhow::Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()?,
            api: api.to_string(),
        })
    }

    pub async fn register(
        &self,
        key: &str,
        req: &RegisterRequest,
    ) -> Result<RegisterResponse, ClientError> {
        let res = self
            .http
            .post(format!("{}/api/v1/agent/register", self.api))
            .bearer_auth(key)
            .json(req)
            .send()
            .await?;
        match res.status() {
            s if s.is_success() => Ok(res.json().await?),
            StatusCode::UNAUTHORIZED => Err(ClientError::Unauthorized),
            s => Err(ClientError::Other(anyhow::anyhow!("register failed: {s}"))),
        }
    }

    pub async fn heartbeat(&self, token: &str, hb: &Heartbeat) -> Result<(), ClientError> {
        let res = self
            .http
            .post(format!("{}/api/v1/agent/heartbeat", self.api))
            .bearer_auth(token)
            .json(hb)
            .send()
            .await?;
        match res.status() {
            s if s.is_success() => Ok(()),
            StatusCode::UNAUTHORIZED => Err(ClientError::Unauthorized),
            s => Err(ClientError::Other(anyhow::anyhow!("heartbeat failed: {s}"))),
        }
    }

    pub async fn usage(&self, token: &str, sample: &UsageSample) -> Result<UsageAck, ClientError> {
        let res = self
            .http
            .post(format!("{}/api/v1/agent/usage", self.api))
            .bearer_auth(token)
            .json(sample)
            .send()
            .await?;
        match res.status() {
            s if s.is_success() => Ok(res.json().await?),
            StatusCode::UNAUTHORIZED => Err(ClientError::Unauthorized),
            s => Err(ClientError::Other(anyhow::anyhow!("usage failed: {s}"))),
        }
    }
}
