//! Shared test helpers: a scripted mock transport and a minimal executor.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::VecDeque;
use std::future::Future;
use std::sync::Mutex;
use std::task::{Context, Poll, Waker};

use calcraft_client::{
    ClientOptions, Credentials, DavClient, HttpTransport, Method, Request, Response,
    TransportError, TransportErrorKind,
};

pub const BASE: &str = "https://cal.example.com/";
pub const PASSWORD: &str = "app password 4711";

/// One scripted exchange.
pub struct Exchange {
    method: Method,
    /// Expected path, or a full URL when it starts with `http`.
    target: String,
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// What the client sent.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    pub authorization: Option<String>,
    pub body: String,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Default)]
pub struct MockTransport {
    script: Mutex<VecDeque<Exchange>>,
    log: Mutex<Vec<Recorded>>,
    /// Simulates a broken transport that ignores `max_response_bytes`.
    pub ignore_limit: bool,
}

impl MockTransport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reply(
        self,
        method: Method,
        target: &str,
        status: u16,
        body: impl Into<Vec<u8>>,
    ) -> Self {
        self.reply_with(method, target, status, &[], body)
    }

    pub fn reply_with(
        self,
        method: Method,
        target: &str,
        status: u16,
        headers: &[(&str, &str)],
        body: impl Into<Vec<u8>>,
    ) -> Self {
        let mut all: Vec<(String, String)> = headers
            .iter()
            .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
            .collect();
        let body = body.into();
        if !body.is_empty() && !all.iter().any(|(n, _)| n == "content-type") {
            all.push((
                "Content-Type".to_owned(),
                "application/xml; charset=utf-8".to_owned(),
            ));
        }
        self.script.lock().unwrap().push_back(Exchange {
            method,
            target: target.to_owned(),
            status,
            headers: all,
            body,
        });
        self
    }

    pub fn redirect(self, method: Method, target: &str, status: u16, location: &str) -> Self {
        self.reply_with(
            method,
            target,
            status,
            &[("Location", location)],
            Vec::new(),
        )
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.log.lock().unwrap().clone()
    }

    pub fn remaining(&self) -> usize {
        self.script.lock().unwrap().len()
    }

    fn answer(&self, request: Request) -> Result<Response, TransportError> {
        let url = request.url.to_string();
        self.log.lock().unwrap().push(Recorded {
            method: request.method,
            url: url.clone(),
            headers: request.headers.clone(),
            authorization: request
                .authorization
                .as_ref()
                .map(|s| s.expose().to_owned()),
            body: String::from_utf8(request.body.clone()).unwrap(),
        });
        let exchange = self
            .script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("unexpected request {:?} {url}", request.method));
        let matches = if exchange.target.starts_with("http") {
            url == exchange.target
        } else {
            request.url.path() == exchange.target
        };
        assert!(
            matches && exchange.method == request.method,
            "expected {:?} {}, got {:?} {url}",
            exchange.method,
            exchange.target,
            request.method
        );
        if !self.ignore_limit && exchange.body.len() > request.max_response_bytes {
            return Err(TransportError::new(
                TransportErrorKind::BodyTooLarge,
                "mock body too large",
            ));
        }
        Ok(Response {
            status: exchange.status,
            headers: exchange.headers,
            body: exchange.body,
        })
    }
}

impl HttpTransport for MockTransport {
    fn send(
        &self,
        request: Request,
    ) -> impl Future<Output = Result<Response, TransportError>> + Send {
        std::future::ready(self.answer(request))
    }
}

pub fn credentials() -> Credentials {
    Credentials::basic("alice", PASSWORD).unwrap()
}

pub fn client(base: &str, mock: MockTransport) -> DavClient<MockTransport> {
    DavClient::new(base, credentials(), mock).unwrap()
}

pub fn client_with(
    base: &str,
    mock: MockTransport,
    options: ClientOptions,
) -> DavClient<MockTransport> {
    DavClient::with_options(base, credentials(), mock, options).unwrap()
}

/// Runs a future that never waits (the mock transport is synchronous).
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut cx) {
        Poll::Ready(output) => output,
        Poll::Pending => panic!("future unexpectedly pending"),
    }
}

/// Every request went to `origin` and carried Basic credentials.
pub fn assert_same_origin(mock: &MockTransport, origin: &str) {
    for request in mock.requests() {
        assert!(
            request.url.starts_with(origin),
            "request left the origin: {}",
            request.url
        );
        assert!(
            request
                .authorization
                .as_deref()
                .is_some_and(|a| a.starts_with("Basic ")),
            "missing credentials on {}",
            request.url
        );
    }
}
