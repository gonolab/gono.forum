//! Minimal client for outgoing HTTPS requests, built on the hyper family of crates already in the dependency tree
//! (no reqwest).
//! Used for SMS providers and third-party sign-in (Google, GitHub).
//!
//! - HTTPS only: the connector is `https_only`; a plain-text address is an error.
//! - Uses the ring crypto backend explicitly (aws-lc-rs is also in the dependency tree, and rustls 0.23 cannot rely
//!   on the default backend when both are present); root certificates come from the bundled webpki-roots.
//! - Every request has an overall timeout and the response body has a limit; reaching it is an error, so a large
//!   response from the other side is never read into memory.
//! - The client has a connection pool; only one is built per process.
use std::{sync::OnceLock, time::Duration};

use anyhow::{Context, Result, anyhow};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper_rustls::HttpsConnector;
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};

pub use hyper::{Method, Request, header};

/// Response: the status code and the complete response body.
pub struct Response {
    pub status: u16,
    /// The `Location` header, for callers that follow redirects themselves (the update download).
    pub location: Option<String>,
    pub body: Bytes,
}

type Inner = Client<HttpsConnector<HttpConnector>, Full<Bytes>>;
type Plain = Client<HttpConnector, Full<Bytes>>;

fn client() -> Result<&'static Inner> {
    static CLIENT: OnceLock<Inner> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client);
    }
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_provider_and_webpki_roots(provider)
        .context("failed to initialize the HTTPS client")?
        .https_only()
        .enable_http1()
        .build();
    let built = Client::builder(TokioExecutor::new()).build(https);
    Ok(CLIENT.get_or_init(|| built))
}

/// Sends one request and reads the whole response body. `timeout` covers everything from connecting to finishing
/// the read; `limit` is the byte limit of the response body.
pub async fn send(
    request: Request<Full<Bytes>>,
    timeout: Duration,
    limit: usize,
) -> Result<Response> {
    let client = client()?;
    exchange(client.request(request), timeout, limit).await
}

/// A local fake provider for tests: plain HTTP, only to `127.0.0.1`. Production addresses are all HTTPS constants
/// and go through [`send`]; switching to a local address can only be done in code
/// ([`crate::oauth::Endpoints::loopback`]), not in the configuration file.
pub async fn send_loopback(
    request: Request<Full<Bytes>>,
    timeout: Duration,
    limit: usize,
) -> Result<Response> {
    let uri = request.uri();
    anyhow::ensure!(
        uri.scheme_str() == Some("http") && uri.host() == Some("127.0.0.1"),
        "plain HTTP requests may only go to 127.0.0.1"
    );
    static PLAIN: OnceLock<Plain> = OnceLock::new();
    let client = PLAIN.get_or_init(|| Client::builder(TokioExecutor::new()).build_http());
    exchange(client.request(request), timeout, limit).await
}

async fn exchange<F>(request: F, timeout: Duration, limit: usize) -> Result<Response>
where
    F: std::future::Future<
            Output = std::result::Result<
                hyper::Response<hyper::body::Incoming>,
                hyper_util::client::legacy::Error,
            >,
        >,
{
    let exchange = async {
        let response = request.await.context("failed to send the request")?;
        let status = response.status().as_u16();
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = Limited::new(response.into_body(), limit)
            .collect()
            .await
            .map_err(|error| anyhow!("failed to read the response: {error}"))?
            .to_bytes();
        Ok::<_, anyhow::Error>(Response {
            status,
            location,
            body,
        })
    };
    tokio::time::timeout(timeout, exchange)
        .await
        .map_err(|_| anyhow!("request timed out ({} seconds)", timeout.as_secs()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn plain_http_is_refused_before_connecting() {
        let request = Request::builder()
            .method(Method::GET)
            .uri("http://127.0.0.1:9/")
            .body(Full::new(Bytes::new()))
            .unwrap();
        assert!(send(request, Duration::from_secs(2), 1024).await.is_err());
        let elsewhere = Request::builder()
            .method(Method::GET)
            .uri("http://10.0.0.1:9/")
            .body(Full::new(Bytes::new()))
            .unwrap();
        let error = send_loopback(elsewhere, Duration::from_secs(2), 1024)
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("127.0.0.1"));
    }
}
