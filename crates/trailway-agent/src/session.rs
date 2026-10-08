//! The agent's WebSocket to the API: receives deploy jobs, sends status and logs.

use std::{sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc::unbounded_channel;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Error as WsError, Message},
};
use trailway_proto::{AgentMessage, ApiMessage, AGENT_WS_PATH};

use crate::deploy::Manager;

const REFRESH_EVERY: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub enum SessionError {
    /// The API rejected the server token (401).
    Unauthorized,
    Other(anyhow::Error),
}

impl From<anyhow::Error> for SessionError {
    fn from(e: anyhow::Error) -> Self {
        Self::Other(e)
    }
}

/// `http(s)://host` to `ws(s)://host/api/v1/agent/ws`.
pub fn ws_url(api: &str) -> anyhow::Result<String> {
    let rest = if let Some(r) = api.strip_prefix("https://") {
        format!("wss://{r}")
    } else if let Some(r) = api.strip_prefix("http://") {
        format!("ws://{r}")
    } else {
        anyhow::bail!("api must be an http(s) URL");
    };
    Ok(format!("{}{AGENT_WS_PATH}", rest.trim_end_matches('/')))
}

/// One connection: runs until the API closes it or something breaks.
pub async fn run_once(api: &str, token: &str, manager: &Arc<Manager>) -> Result<(), SessionError> {
    let mut request = ws_url(api)?
        .into_client_request()
        .map_err(anyhow::Error::from)?;
    let auth = HeaderValue::from_str(&format!("Bearer {token}")).map_err(anyhow::Error::from)?;
    request.headers_mut().insert("Authorization", auth);
    let (socket, _) = match connect_async(request).await {
        Ok(ok) => ok,
        Err(WsError::Http(res)) if res.status() == 401 => return Err(SessionError::Unauthorized),
        Err(e) => return Err(SessionError::Other(e.into())),
    };
    tracing::info!("connected to the API");
    let (mut write, mut read) = socket.split();
    let (sink, mut outgoing) = unbounded_channel::<AgentMessage>();
    manager.attach(sink);

    let mut refresh = tokio::time::interval(REFRESH_EVERY);
    let result = loop {
        tokio::select! {
            incoming = read.next() => match incoming {
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<ApiMessage>(text.as_str()) {
                    Ok(msg) => manager.submit(msg),
                    Err(e) => tracing::warn!("bad message from the API: {e}"),
                },
                Some(Ok(Message::Close(_))) | None => break Ok(()),
                Some(Ok(_)) => {}
                Some(Err(e)) => break Err(SessionError::Other(e.into())),
            },
            Some(msg) = outgoing.recv() => {
                let json = serde_json::to_string(&msg).map_err(anyhow::Error::from)?;
                if let Err(e) = write.send(Message::Text(json.into())).await {
                    break Err(SessionError::Other(e.into()));
                }
            }
            _ = refresh.tick() => {
                let m = manager.clone();
                let _ = tokio::task::spawn_blocking(move || m.refresh()).await;
            }
        }
    };
    manager.detach();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_websocket_urls() {
        assert_eq!(
            ws_url("http://127.0.0.1:8080").unwrap(),
            "ws://127.0.0.1:8080/api/v1/agent/ws"
        );
        assert_eq!(
            ws_url("https://api.example.com/").unwrap(),
            "wss://api.example.com/api/v1/agent/ws"
        );
        assert!(ws_url("ftp://x").is_err());
    }
}
