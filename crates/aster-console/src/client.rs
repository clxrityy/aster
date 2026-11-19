//! Thin HTTP + SSE client for the owner API. The console holds no state the
//! server does not also have; everything is re-fetched on reconnect.

use std::time::Duration;

use futures_util::StreamExt;
use reqwest::{Method, header};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::app::Msg;

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl Client {
    pub fn new(base: &str, token: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: base.trim_end_matches('/').to_string(),
            token,
        }
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<reqwest::Response, String> {
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.base))
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
            .timeout(Duration::from_secs(60));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .map_err(|_| "server unreachable (is `aster serve` running?)".to_string())?;
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status();
        let detail = response.json::<Value>().await.unwrap_or(Value::Null);
        let code = detail["error"].as_str().unwrap_or("error");
        Err(match detail["detail"].as_str() {
            Some(extra) => format!("{} {code}: {extra}", status.as_u16()),
            None => format!("{} {code}", status.as_u16()),
        })
    }

    pub async fn json(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, String> {
        let response = self.send(method, path, body).await?;
        if response.status() == reqwest::StatusCode::NO_CONTENT {
            return Ok(Value::Null);
        }
        response
            .json()
            .await
            .map_err(|_| "malformed server response".into())
    }

    pub async fn get(&self, path: &str) -> Result<Value, String> {
        self.json(Method::GET, path, None).await
    }

    pub async fn post(&self, path: &str, body: Value) -> Result<Value, String> {
        self.json(Method::POST, path, Some(body)).await
    }

    /// Follow `/v1/events` forever, reconnecting with backoff.
    pub fn spawn_events(&self, tx: UnboundedSender<Msg>) {
        let client = self.clone();
        tokio::spawn(async move {
            let mut backoff = Duration::from_millis(500);
            loop {
                match client.send(Method::GET, "/v1/events", None).await {
                    Ok(response) => {
                        backoff = Duration::from_millis(500);
                        if tx.send(Msg::Connected(true)).is_err() {
                            return;
                        }
                        let mut stream = response.bytes_stream();
                        let mut buffer = String::new();
                        while let Some(Ok(chunk)) = stream.next().await {
                            buffer.push_str(&String::from_utf8_lossy(&chunk));
                            while let Some(end) = buffer.find("\n\n") {
                                let block: String = buffer.drain(..end + 2).collect();
                                let data: String = block
                                    .lines()
                                    .filter_map(|line| line.strip_prefix("data:"))
                                    .map(str::trim_start)
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                if data.is_empty() {
                                    continue;
                                }
                                if let Ok(value) = serde_json::from_str::<Value>(&data)
                                    && tx.send(Msg::Event(value)).is_err()
                                {
                                    return;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        // Report only the first failure of a disconnect streak.
                        if backoff == Duration::from_millis(500)
                            && tx.send(Msg::Notice(error, true)).is_err()
                        {
                            return;
                        }
                    }
                }
                if tx.send(Msg::Connected(false)).is_err() {
                    return;
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(5));
                // Re-sync state the stream may have missed while disconnected.
                if tx.send(Msg::Resync).is_err() {
                    return;
                }
            }
        });
    }
}
