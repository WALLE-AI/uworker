use std::time::Duration;

use futures::StreamExt;
use reqwest::redirect::Policy;
use reqwest::{Client, StatusCode};
use tokio_util::sync::CancellationToken;
use url::Url;

use agentrs_config::web::WebConfig;

use crate::web::url_policy::{RejectReason, UrlPolicy};

/// A body that was fetched in full.
#[derive(Debug, Clone)]
pub struct FetchedPage {
    pub final_url: Url,
    pub status: u16,
    pub status_text: String,
    pub content_type: String,
    pub body: Vec<u8>,
}

/// A redirect that policy refused to follow silently.
#[derive(Debug, Clone)]
pub struct BlockedRedirect {
    pub original: Url,
    pub target: Url,
    pub status: u16,
}

#[derive(Debug)]
pub enum FetchOutcome {
    Page(FetchedPage),
    Redirect(BlockedRedirect),
}

#[derive(Debug)]
pub enum FetchError {
    Rejected(RejectReason),
    /// The caller cancelled the turn.
    Cancelled,
    TooManyRedirects {
        limit: usize,
    },
    MissingLocationHeader,
    ContentTooLarge {
        limit: u64,
    },
    Timeout {
        seconds: u64,
    },
    Transport(String),
    HttpStatus {
        status: u16,
        status_text: String,
    },
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(reason) => write!(f, "{reason}"),
            Self::Cancelled => write!(f, "Fetch cancelled before it completed"),
            Self::TooManyRedirects { limit } => {
                write!(f, "Too many redirects (exceeded {limit})")
            }
            Self::MissingLocationHeader => write!(f, "Redirect missing Location header"),
            Self::ContentTooLarge { limit } => {
                write!(f, "Response body exceeds the {limit} byte limit")
            }
            Self::Timeout { seconds } => write!(f, "Request timed out after {seconds}s"),
            Self::Transport(message) => write!(f, "Request failed: {message}"),
            Self::HttpStatus { status, status_text } => {
                write!(f, "Request failed with HTTP {status} {status_text}")
            }
        }
    }
}

/// HTTP client for `WebFetch`.
///
/// Redirects are followed by hand (reqwest's own follower is disabled) so each
/// hop passes through [`UrlPolicy`] and a cross-host jump can be reported back
/// to the model instead of silently retargeting the request.
pub struct HttpClient {
    client: Client,
    policy: UrlPolicy,
    max_redirects: usize,
    max_content_bytes: u64,
    timeout: Duration,
}

impl HttpClient {
    pub fn new(config: &WebConfig, policy: UrlPolicy) -> Result<Self, String> {
        let mut builder = Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(config.timeout_secs))
            .user_agent(config.effective_user_agent());

        // Loopback and LAN targets are not reachable through an egress proxy,
        // and reqwest — unlike most browsers — does not bypass one for them.
        // Honouring `HTTP_PROXY` here would send every private-network fetch to
        // a proxy that cannot route it.
        if config.allow_private_network {
            builder = builder.no_proxy();
        }

        let client = builder
            .build()
            .map_err(|error| format!("failed to build HTTP client: {error}"))?;

        Ok(Self {
            client,
            policy,
            max_redirects: config.max_redirects,
            max_content_bytes: config.max_content_bytes,
            timeout: Duration::from_secs(config.timeout_secs),
        })
    }

    /// Fetch `raw_url`, following only redirects the policy permits.
    pub async fn fetch(&self, raw_url: &str, cancel: &CancellationToken) -> Result<FetchOutcome, FetchError> {
        let start = self.policy.check(raw_url).map_err(FetchError::Rejected)?;
        let mut current = start;

        for _ in 0..=self.max_redirects {
            if cancel.is_cancelled() {
                return Err(FetchError::Cancelled);
            }
            self.policy
                .check_resolved(&current)
                .await
                .map_err(FetchError::Rejected)?;

            let response = self.send(&current, cancel).await?;
            let status = response.status();

            if !status.is_redirection() {
                if !status.is_success() {
                    return Err(FetchError::HttpStatus {
                        status: status.as_u16(),
                        status_text: status_text(status),
                    });
                }
                return self.read_body(current, response, cancel).await.map(FetchOutcome::Page);
            }

            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or(FetchError::MissingLocationHeader)?;
            let target = current.join(location).map_err(|_| FetchError::MissingLocationHeader)?;

            if !self.policy.is_permitted_redirect(&current, &target) {
                return Ok(FetchOutcome::Redirect(BlockedRedirect {
                    original: current,
                    target,
                    status: status.as_u16(),
                }));
            }
            // A permitted redirect still re-enters the host policy on the next
            // iteration, so a same-host hop cannot walk into a private address.
            self.policy.check(target.as_str()).map_err(FetchError::Rejected)?;
            current = target;
        }

        Err(FetchError::TooManyRedirects {
            limit: self.max_redirects,
        })
    }

    async fn send(&self, url: &Url, cancel: &CancellationToken) -> Result<reqwest::Response, FetchError> {
        let request = self
            .client
            .get(url.clone())
            .header(reqwest::header::ACCEPT, "text/markdown, text/html, */*")
            .send();

        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(FetchError::Cancelled),
            result = request => result.map_err(|error| self.classify(error)),
        }
    }

    /// Stream the body, aborting as soon as the limit is passed.
    ///
    /// Reading to completion first would let a hostile server force us to buy
    /// the whole transfer before we could reject it.
    async fn read_body(
        &self,
        final_url: Url,
        response: reqwest::Response,
        cancel: &CancellationToken,
    ) -> Result<FetchedPage, FetchError> {
        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();

        if let Some(declared) = response.content_length()
            && declared > self.max_content_bytes
        {
            return Err(FetchError::ContentTooLarge {
                limit: self.max_content_bytes,
            });
        }

        let mut body: Vec<u8> = Vec::new();
        let mut stream = response.bytes_stream();
        loop {
            let chunk = tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(FetchError::Cancelled),
                chunk = stream.next() => chunk,
            };
            let Some(chunk) = chunk else { break };
            let chunk = chunk.map_err(|error| self.classify(error))?;
            if body.len() as u64 + chunk.len() as u64 > self.max_content_bytes {
                return Err(FetchError::ContentTooLarge {
                    limit: self.max_content_bytes,
                });
            }
            body.extend_from_slice(&chunk);
        }

        Ok(FetchedPage {
            final_url,
            status: status.as_u16(),
            status_text: status_text(status),
            content_type,
            body,
        })
    }

    fn classify(&self, error: reqwest::Error) -> FetchError {
        if error.is_timeout() {
            return FetchError::Timeout {
                seconds: self.timeout.as_secs(),
            };
        }
        // The message can carry the full URL including a query string; keep it
        // for the model but never log it at info level or above.
        FetchError::Transport(error.to_string())
    }
}

fn status_text(status: StatusCode) -> String {
    status.canonical_reason().unwrap_or("Unknown").to_string()
}

#[cfg(test)]
#[path = "http_client_test.rs"]
mod http_client_test;
