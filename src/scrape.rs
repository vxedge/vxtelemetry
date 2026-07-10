//! Plain-TCP HTTP/1.1 GET of an agent's /metrics endpoint. Hand-rolled on
//! purpose: agents expose trivial HTTP, and a full HTTP client stack would
//! violate the lightweight rule for no benefit.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{Duration, timeout};

use crate::TelemetryError;

const SCRAPE_TIMEOUT: Duration = Duration::from_secs(3);

/// Fetch `http://<target>/metrics` and return the body.
pub async fn fetch_metrics(target: &str) -> Result<String, TelemetryError> {
    let err = |detail: String| TelemetryError::Scrape {
        target: target.to_string(),
        detail,
    };
    let fut = async {
        let mut stream = TcpStream::connect(target)
            .await
            .map_err(|e| err(e.to_string()))?;
        let req = format!(
            "GET /metrics HTTP/1.1\r\nhost: {target}\r\nconnection: close\r\nuser-agent: vxtelemetry\r\n\r\n"
        );
        stream
            .write_all(req.as_bytes())
            .await
            .map_err(|e| err(e.to_string()))?;
        let mut raw = Vec::with_capacity(4096);
        stream
            .read_to_end(&mut raw)
            .await
            .map_err(|e| err(e.to_string()))?;
        let text = String::from_utf8_lossy(&raw);
        let Some((head, body)) = text.split_once("\r\n\r\n") else {
            return Err(err("malformed HTTP response (no header/body split)".into()));
        };
        let status = head.lines().next().unwrap_or_default();
        if !status.contains(" 200 ") {
            return Err(err(format!("non-200 response: {status}")));
        }
        Ok(body.to_string())
    };
    timeout(SCRAPE_TIMEOUT, fut)
        .await
        .map_err(|_| err(format!("timeout after {SCRAPE_TIMEOUT:?}")))?
}
