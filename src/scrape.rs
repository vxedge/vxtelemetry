//! Plain-TCP HTTP/1.1 GET of an agent's /metrics endpoint. Hand-rolled on
//! purpose: agents expose trivial HTTP, and a full HTTP client stack would
//! violate the lightweight rule for no benefit.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{Duration, timeout};

use crate::TelemetryError;

const SCRAPE_TIMEOUT: Duration = Duration::from_secs(3);

/// Split a scrape target, `host:port` or `host:port/path`, into the address to connect to and
/// the request path: `<path>/metrics`, or `/metrics` with no path. The daemon serves each run's
/// exporter at `/runs/<slot>/metrics` beside `/metrics`, so two concurrent runs' archives scrape
/// only their own series.
fn split_target(target: &str) -> (&str, String) {
    match target.split_once('/') {
        Some((addr, path)) => {
            let path = path.trim_matches('/');
            if path.is_empty() {
                (addr, "/metrics".to_string())
            } else {
                (addr, format!("/{path}/metrics"))
            }
        }
        None => (target, "/metrics".to_string()),
    }
}

/// Fetch `http://<target>/metrics`, or `http://<host:port>/<path>/metrics` for a target with a
/// path, and return the body.
pub async fn fetch_metrics(target: &str) -> Result<String, TelemetryError> {
    let (addr, path) = split_target(target);
    let err = |detail: String| TelemetryError::Scrape {
        target: target.to_string(),
        detail,
    };
    let fut = async {
        let mut stream = TcpStream::connect(addr)
            .await
            .map_err(|e| err(e.to_string()))?;
        let req = format!(
            "GET {path} HTTP/1.1\r\nhost: {addr}\r\nconnection: close\r\nuser-agent: vxtelemetry\r\n\r\n"
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

#[cfg(test)]
mod tests {
    use super::split_target;

    #[test]
    fn a_target_may_carry_a_path_before_metrics() {
        assert_eq!(
            split_target("127.0.0.1:9801"),
            ("127.0.0.1:9801", "/metrics".into())
        );
        assert_eq!(
            split_target("127.0.0.1:9100/runs/1"),
            ("127.0.0.1:9100", "/runs/1/metrics".into())
        );
        assert_eq!(
            split_target("127.0.0.1:9100/runs/1/"),
            ("127.0.0.1:9100", "/runs/1/metrics".into())
        );
        assert_eq!(
            split_target("127.0.0.1:9100/"),
            ("127.0.0.1:9100", "/metrics".into())
        );
    }
}
