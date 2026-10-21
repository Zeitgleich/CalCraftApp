//! Default transport: hyper 1 (HTTP/1.1, connection pooling via
//! `hyper-util`'s legacy client) with rustls (ring provider, TLS 1.2/1.3)
//! and the Mozilla root store from `webpki-roots`, plus optional extra roots
//! for self-hosted servers with a private CA.
//!
//! Must be used from within a Tokio runtime.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioTimer};
use rustls::RootCertStore;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;

use crate::transport::{HttpTransport, Request, Response, TransportError, TransportErrorKind};
use crate::url::Scheme;

const USER_AGENT: &str = concat!("calcraft-client/", env!("CARGO_PKG_VERSION"));

/// Builder for [`HyperTransport`].
#[derive(Debug, Clone)]
pub struct HyperTransportBuilder {
    extra_roots: Vec<CertificateDer<'static>>,
    webpki_roots: bool,
    timeout: Duration,
    connect_timeout: Duration,
    allow_insecure_http: bool,
}

impl Default for HyperTransportBuilder {
    fn default() -> Self {
        Self {
            extra_roots: Vec::new(),
            webpki_roots: true,
            timeout: Duration::from_secs(60),
            connect_timeout: Duration::from_secs(15),
            allow_insecure_http: false,
        }
    }
}

impl HyperTransportBuilder {
    /// Trusts additional CA certificates (PEM, one or more), for example a
    /// self-hosted server's private CA.
    ///
    /// # Errors
    /// If the PEM data contains no valid certificate.
    pub fn add_root_certificates_pem(mut self, pem: &[u8]) -> Result<Self, TransportError> {
        let before = self.extra_roots.len();
        for cert in CertificateDer::pem_slice_iter(pem) {
            let cert = cert.map_err(|e| {
                TransportError::new(TransportErrorKind::Other, format!("invalid PEM: {e}"))
            })?;
            self.extra_roots.push(cert);
        }
        if self.extra_roots.len() == before {
            return Err(TransportError::new(
                TransportErrorKind::Other,
                "no certificate in PEM data",
            ));
        }
        Ok(self)
    }

    /// Whether to trust the bundled Mozilla root store (default: yes). Turn
    /// off to trust only certificates added with
    /// [`add_root_certificates_pem`](Self::add_root_certificates_pem).
    #[must_use]
    pub fn webpki_roots(mut self, enabled: bool) -> Self {
        self.webpki_roots = enabled;
        self
    }

    /// Total timeout per request including reading the body (default 60 s).
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// TCP connect timeout (default 15 s).
    #[must_use]
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Allow plain HTTP to non-loopback hosts (default: refused).
    #[must_use]
    pub fn allow_insecure_http(mut self, allow: bool) -> Self {
        self.allow_insecure_http = allow;
        self
    }

    /// Builds the transport.
    ///
    /// # Errors
    /// If the TLS configuration cannot be built or no root is trusted.
    pub fn build(self) -> Result<HyperTransport, TransportError> {
        let mut roots = RootCertStore::empty();
        if self.webpki_roots {
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        }
        for cert in self.extra_roots {
            roots.add(cert).map_err(|e| {
                TransportError::new(TransportErrorKind::Other, format!("invalid CA: {e}"))
            })?;
        }
        if roots.is_empty() {
            return Err(TransportError::new(
                TransportErrorKind::Other,
                "no trusted root certificates",
            ));
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| TransportError::new(TransportErrorKind::Other, e.to_string()))?
            .with_root_certificates(roots)
            .with_no_client_auth();
        let mut http = HttpConnector::new();
        http.enforce_http(false);
        http.set_connect_timeout(Some(self.connect_timeout));
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls)
            .https_or_http()
            .enable_http1()
            .wrap_connector(http);
        let client = Client::builder(TokioExecutor::new())
            .pool_timer(TokioTimer::new())
            .pool_idle_timeout(Duration::from_secs(30))
            .build(https);
        Ok(HyperTransport {
            client,
            timeout: self.timeout,
            allow_insecure_http: self.allow_insecure_http,
        })
    }
}

/// The default [`HttpTransport`]. Cheap to clone (shared connection pool).
#[derive(Clone)]
pub struct HyperTransport {
    client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
    timeout: Duration,
    allow_insecure_http: bool,
}

impl std::fmt::Debug for HyperTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HyperTransport")
            .field("timeout", &self.timeout)
            .field("allow_insecure_http", &self.allow_insecure_http)
            .finish_non_exhaustive()
    }
}

impl HyperTransport {
    /// A transport with default settings.
    ///
    /// # Errors
    /// If the TLS configuration cannot be built.
    pub fn new() -> Result<Self, TransportError> {
        HyperTransportBuilder::default().build()
    }

    /// A builder for custom settings.
    #[must_use]
    pub fn builder() -> HyperTransportBuilder {
        HyperTransportBuilder::default()
    }

    async fn execute(&self, request: Request) -> Result<Response, TransportError> {
        let refused = |message: String| TransportError::new(TransportErrorKind::Refused, message);
        if request.url.scheme() == Scheme::Http
            && !request.url.is_loopback()
            && !self.allow_insecure_http
        {
            return Err(refused(format!(
                "unencrypted HTTP to {}",
                request.url.host()
            )));
        }
        let method = http::Method::from_bytes(request.method.as_str().as_bytes())
            .map_err(|e| refused(e.to_string()))?;
        let mut builder = http::Request::builder()
            .method(method)
            .uri(request.url.to_string())
            .header(http::header::USER_AGENT, USER_AGENT);
        for (name, value) in &request.headers {
            builder = builder.header(*name, value.as_str());
        }
        let mut http_request = builder
            .body(Full::new(Bytes::from(request.body)))
            .map_err(|e| refused(e.to_string()))?;
        if let Some(secret) = &request.authorization {
            let mut value = http::HeaderValue::from_str(secret.expose())
                .map_err(|_| refused("invalid authorization header".into()))?;
            value.set_sensitive(true);
            http_request
                .headers_mut()
                .insert(http::header::AUTHORIZATION, value);
        }
        drop(request.authorization);

        let response = self
            .client
            .request(http_request)
            .await
            .map_err(|e| TransportError::new(TransportErrorKind::Connect, error_chain(&e)))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned()))
            })
            .collect::<Vec<_>>();
        let limit = request.max_response_bytes;
        let too_large = || {
            TransportError::new(
                TransportErrorKind::BodyTooLarge,
                format!("response body exceeds {limit} bytes"),
            )
        };
        if let Some(length) = response
            .headers()
            .get(http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            && length > limit as u64
        {
            return Err(too_large());
        }
        let mut body = response.into_body();
        let mut buffer = Vec::new();
        while let Some(frame) = body.frame().await {
            let frame = frame
                .map_err(|e| TransportError::new(TransportErrorKind::Other, error_chain(&e)))?;
            if let Ok(data) = frame.into_data() {
                if buffer.len() + data.len() > limit {
                    return Err(too_large());
                }
                buffer.extend_from_slice(&data);
            }
        }
        Ok(Response {
            status,
            headers,
            body: buffer,
        })
    }
}

impl HttpTransport for HyperTransport {
    async fn send(&self, request: Request) -> Result<Response, TransportError> {
        tokio::time::timeout(self.timeout, self.execute(request))
            .await
            .map_err(|_| TransportError::new(TransportErrorKind::Timeout, "request timed out"))?
    }
}

fn error_chain(error: &dyn std::error::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(inner) = source {
        message.push_str(": ");
        message.push_str(&inner.to_string());
        source = inner.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::Method;
    use crate::url::Url;

    fn request(url: &str) -> Request {
        Request {
            method: Method::Propfind,
            url: Url::parse(url).unwrap(),
            headers: Vec::new(),
            authorization: None,
            body: Vec::new(),
            max_response_bytes: 1024,
        }
    }

    #[test]
    fn refuses_plain_http_to_remote_hosts() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let transport = HyperTransport::new().unwrap();
        let error = runtime
            .block_on(transport.send(request("http://cal.example.com/dav/")))
            .unwrap_err();
        assert_eq!(error.kind, TransportErrorKind::Refused);
    }

    #[test]
    fn root_configuration() {
        assert!(
            HyperTransport::builder()
                .add_root_certificates_pem(b"not a certificate")
                .is_err()
        );
        assert!(
            HyperTransport::builder()
                .webpki_roots(false)
                .build()
                .is_err()
        );
    }
}
