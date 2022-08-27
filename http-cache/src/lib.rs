#![forbid(unsafe_code, future_incompatible)]
#![deny(
    missing_docs,
    missing_debug_implementations,
    missing_copy_implementations,
    nonstandard_style,
    unused_qualifications,
    unused_import_braces,
    unused_extern_crates,
    trivial_casts,
    trivial_numeric_casts
)]
#![cfg_attr(docsrs, feature(doc_cfg))]
//! A caching middleware that follows HTTP caching rules, thanks to
//! [`http-cache-semantics`](https://github.com/kornelski/rusty-http-cache-semantics).
//! By default, it uses [`cacache`](https://github.com/zkat/cacache-rs) as the backend cache manager.
//!
//! ## Features
//!
//! The following features are available. By default `manager-cacache` is enabled.
//!
//! - `manager-cacache` (default): enable [cacache](https://github.com/zkat/cacache-rs),
//! a high-performance disk cache, backend manager.
//! - `manager-moka` (disabled): enable [moka](https://github.com/moka-rs/moka),
//! a high-performance in-memory cache, backend manager.
//! - `with-http-types` (disabled): enable [http-types](https://github.com/http-rs/http-types)
//! type conversion support
mod error;
mod managers;

use std::{
    collections::HashMap, convert::TryFrom, fmt, str::FromStr, time::SystemTime,
};

use http::{header::CACHE_CONTROL, request, response, StatusCode};
use http_cache_semantics::{AfterResponse, BeforeRequest, CachePolicy};
use serde::{Deserialize, Serialize};
use url::Url;

pub use error::{BadHeader, BadVersion, BoxError, Result};

#[cfg(feature = "manager-cacache")]
pub use managers::cacache::CACacheManager;

#[cfg(feature = "manager-moka")]
pub use managers::moka::MokaManager;

// Exposing the moka cache for convenience, renaming to avoid naming conflicts
#[cfg(feature = "manager-moka")]
#[cfg_attr(docsrs, doc(cfg(feature = "manager-moka")))]
pub use moka::future::{Cache as MokaCache, CacheBuilder as MokaCacheBuilder};

// Custom headers used to indicate cache status (hit or miss)
/// `x-cache` header: Value will be HIT if the response was served from cache, MISS if not
pub const XCACHE: &str = "x-cache";
/// `x-cache-lookup` header: Value will be HIT if a response existed in cache, MISS if not
pub const XCACHELOOKUP: &str = "x-cache-lookup";

/// Represents a basic cache status
/// Used in the custom headers `x-cache` and `x-cache-lookup`
#[derive(Debug, Copy, Clone)]
pub enum HitOrMiss {
    /// Yes, there was a hit
    HIT,
    /// No, there was no hit
    MISS,
}

impl fmt::Display for HitOrMiss {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::HIT => write!(f, "HIT"),
            Self::MISS => write!(f, "MISS"),
        }
    }
}

/// Represents an HTTP version
#[derive(Debug, Copy, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub enum HttpVersion {
    /// HTTP Version 0.9
    #[serde(rename = "HTTP/0.9")]
    Http09,
    /// HTTP Version 1.0
    #[serde(rename = "HTTP/1.0")]
    Http10,
    /// HTTP Version 1.1
    #[serde(rename = "HTTP/1.1")]
    Http11,
    /// HTTP Version 2.0
    #[serde(rename = "HTTP/2.0")]
    H2,
    /// HTTP Version 3.0
    #[serde(rename = "HTTP/3.0")]
    H3,
}

/// A basic generic type that represents an HTTP response
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HttpResponse {
    /// HTTP response body
    pub body: Vec<u8>,
    /// HTTP response headers
    pub headers: HashMap<String, String>,
    /// HTTP response status code
    pub status: u16,
    /// HTTP response url
    pub url: Url,
    /// HTTP response version
    pub version: HttpVersion,
}

impl HttpResponse {
    /// Create a new `HttpResponse` instance
    pub fn new(
        body: Vec<u8>,
        headers: HashMap<String, String>,
        status: u16,
        url: Url,
        version: HttpVersion,
    ) -> Self {
        Self { body, headers, status, url, version }
    }

    /// Returns `http::response::Parts`
    pub fn parts(&self) -> Result<response::Parts> {
        let mut converted =
            response::Builder::new().status(self.status).body(())?;
        {
            let headers = converted.headers_mut();
            for header in &self.headers {
                headers.insert(
                    http::header::HeaderName::from_str(header.0.as_str())?,
                    http::HeaderValue::from_str(header.1.as_str())?,
                );
            }
        }
        Ok(converted.into_parts().0)
    }

    /// Returns the status code of the warning header if present
    #[must_use]
    pub fn warning_code(&self) -> Option<usize> {
        self.headers.get("warning").and_then(|hdr| {
            hdr.as_str().chars().take(3).collect::<String>().parse().ok()
        })
    }

    /// Adds a warning header to a response
    pub fn add_warning(&mut self, url: &Url, code: usize, message: &str) {
        // warning    = "warning" ":" 1#warning-value
        // warning-value = warn-code SP warn-agent SP warn-text [SP warn-date]
        // warn-code  = 3DIGIT
        // warn-agent = ( host [ ":" port ] ) | pseudonym
        //                 ; the name or pseudonym of the server adding
        //                 ; the warning header, for use in debugging
        // warn-text  = quoted-string
        // warn-date  = <"> HTTP-date <">
        // (https://tools.ietf.org/html/rfc2616#section-14.46)
        self.headers.insert(
            "warning".to_string(),
            format!(
                "{} {} {:?} \"{}\"",
                code,
                url.host().expect("Invalid URL"),
                message,
                httpdate::fmt_http_date(SystemTime::now())
            ),
        );
    }

    /// Removes a warning header from a response
    pub fn remove_warning(&mut self) {
        self.headers.remove("warning");
    }

    /// Update the headers from `http::response::Parts`
    pub fn update_headers(&mut self, parts: &response::Parts) -> Result<()> {
        for header in parts.headers.iter() {
            self.headers.insert(
                header.0.as_str().to_string(),
                header.1.to_str()?.to_string(),
            );
        }
        Ok(())
    }

    /// Checks if the Cache-Control header contains the must-revalidate directive
    #[must_use]
    pub fn must_revalidate(&self) -> bool {
        self.headers.get(CACHE_CONTROL.as_str()).map_or(false, |val| {
            val.as_str().to_lowercase().contains("must-revalidate")
        })
    }

    /// Adds the custom `x-cache` header to the response
    pub fn cache_status(&mut self, hit_or_miss: HitOrMiss) {
        self.headers.insert(XCACHE.to_string(), hit_or_miss.to_string());
    }

    /// Adds the custom `x-cache-lookup` header to the response
    pub fn cache_lookup_status(&mut self, hit_or_miss: HitOrMiss) {
        self.headers.insert(XCACHELOOKUP.to_string(), hit_or_miss.to_string());
    }
}

impl Default for HttpResponse {
    fn default() -> Self {
        let mut response = Self::new(
            Vec::new(),
            HashMap::default(),
            500,
            Url::parse("http://localhost").unwrap(),
            HttpVersion::Http11,
        );
        response.cache_status(HitOrMiss::MISS);
        response.cache_lookup_status(HitOrMiss::MISS);
        response
    }
}

/// A trait providing methods for storing, reading, and removing cache records.
#[async_trait::async_trait]
pub trait CacheManager {
    /// Attempts to pull a cached response and related policy from cache.
    async fn get(
        &self,
        method: &str,
        url: &Url,
    ) -> Result<Option<(HttpResponse, CachePolicy)>>;
    /// Attempts to cache a response and related policy.
    async fn put(
        &self,
        method: &str,
        url: &Url,
        res: HttpResponse,
        policy: CachePolicy,
    ) -> Result<()>;
    /// Attempts to remove a record from cache.
    async fn delete(&self, method: &str, url: &Url) -> Result<()>;
}

/// Similar to [make-fetch-happen cache options](https://github.com/npm/make-fetch-happen#--optscache).
/// Passed in when the [`HttpCache`] struct is being built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheMode {
    /// Will inspect the HTTP cache on the way to the network.
    /// If there is a fresh response it will be used.
    /// If there is a stale response a conditional request will be created,
    /// and a normal request otherwise.
    /// It then updates the HTTP cache with the response.
    /// If the revalidation request fails (for example, on a 500 or if you're offline),
    /// the stale response will be returned.
    Default,
    /// Behaves as if there is no HTTP cache at all.
    NoStore,
    /// Behaves as if there is no HTTP cache on the way to the network.
    /// Ergo, it creates a normal request and updates the HTTP cache with the response.
    Reload,
    /// Creates a conditional request if there is a response in the HTTP cache
    /// and a normal request otherwise. It then updates the HTTP cache with the response.
    NoCache,
    /// Uses any response in the HTTP cache matching the request,
    /// not paying attention to staleness. If there was no response,
    /// it creates a normal request and updates the HTTP cache with the response.
    ForceCache,
    /// Uses any response in the HTTP cache matching the request,
    /// not paying attention to staleness. If there was no response,
    /// it returns a network error.
    OnlyIfCached,
}

impl TryFrom<http::Version> for HttpVersion {
    type Error = BoxError;

    fn try_from(value: http::Version) -> Result<Self> {
        Ok(match value {
            http::Version::HTTP_09 => Self::Http09,
            http::Version::HTTP_10 => Self::Http10,
            http::Version::HTTP_11 => Self::Http11,
            http::Version::HTTP_2 => Self::H2,
            http::Version::HTTP_3 => Self::H3,
            _ => return Err(Box::new(BadVersion)),
        })
    }
}

impl From<HttpVersion> for http::Version {
    fn from(value: HttpVersion) -> Self {
        match value {
            HttpVersion::Http09 => Self::HTTP_09,
            HttpVersion::Http10 => Self::HTTP_10,
            HttpVersion::Http11 => Self::HTTP_11,
            HttpVersion::H2 => Self::HTTP_2,
            HttpVersion::H3 => Self::HTTP_3,
        }
    }
}

#[cfg(feature = "http-types")]
impl TryFrom<http_types::Version> for HttpVersion {
    type Error = BoxError;

    fn try_from(value: http_types::Version) -> Result<Self> {
        Ok(match value {
            http_types::Version::Http0_9 => Self::Http09,
            http_types::Version::Http1_0 => Self::Http10,
            http_types::Version::Http1_1 => Self::Http11,
            http_types::Version::Http2_0 => Self::H2,
            http_types::Version::Http3_0 => Self::H3,
            _ => return Err(Box::new(BadVersion)),
        })
    }
}

#[cfg(feature = "http-types")]
impl From<HttpVersion> for http_types::Version {
    fn from(value: HttpVersion) -> Self {
        match value {
            HttpVersion::Http09 => Self::Http0_9,
            HttpVersion::Http10 => Self::Http1_0,
            HttpVersion::Http11 => Self::Http1_1,
            HttpVersion::H2 => Self::Http2_0,
            HttpVersion::H3 => Self::Http3_0,
        }
    }
}

/// Represents next step actions to be taken as determined by the cache logic.
#[derive(Debug)]
pub enum Action {
    /// Proceed with a request.
    Remote(Fetch),
    /// Return the cached response.
    Cached {
        /// The cached response.
        response: Box<HttpResponse>,
    },
}

/// Represents the type of fetch being performed.
#[derive(Debug)]
pub enum Fetch {
    /// Proceed with a normal request.
    Normal,
    /// Force Cache-Control to be no-cache then proceed with a normal request.
    ForceNoCache,
    /// A conditional request with stages.
    Conditional(Stage),
}

/// Represents a stage of the conditional request process.
#[derive(Debug)]
pub enum Stage {
    /// Check cache logic before a conditional request.
    BeforeFetch {
        /// The cached response.
        response: Box<HttpResponse>,
        /// The cache policy that corresponds to the request.
        policy: Box<CachePolicy>,
    },
    /// Return the cached response skipping the fetch.
    Cached {
        /// The cached response.
        response: Box<HttpResponse>,
    },
    /// Update the request headers before proceeding with a conditional request.
    UpdateRequestHeaders {
        /// The parts used to update the request before proceeding with a conditional request.
        request_parts: Box<request::Parts>,
    },
}

/// Options struct provided by
/// [`http-cache-semantics`](https://github.com/kornelski/rusty-http-cache-semantics).
pub use http_cache_semantics::CacheOptions;

/// Caches requests according to http spec.
#[derive(Debug, Clone)]
pub struct HttpCache<T: CacheManager> {
    /// Determines the manager behavior.
    pub mode: CacheMode,
    /// Manager instance that implements the [`CacheManager`] trait.
    /// By default, a manager implementation with [`cacache`](https://github.com/zkat/cacache-rs)
    /// as the backend has been provided, see [`CACacheManager`].
    pub manager: T,
    /// Override the default cache options.
    pub options: Option<CacheOptions>,
}

#[allow(dead_code)]
impl<T: CacheManager> HttpCache<T> {
    fn build_policy(
        &self,
        request_parts: &request::Parts,
        response: &HttpResponse,
    ) -> Result<CachePolicy> {
        let policy = match self.options {
            Some(options) => CachePolicy::new_options(
                request_parts,
                &response.parts()?,
                SystemTime::now(),
                options,
            ),
            None => CachePolicy::new(request_parts, &response.parts()?),
        };
        Ok(policy)
    }

    /// Runs before the request is executed and returns the next action to be taken.
    pub async fn before_request(
        &self,
        request_parts: &request::Parts,
    ) -> Result<Action> {
        let is_cacheable = request_parts.method == "GET"
            || request_parts.method == "HEAD"
                && self.mode != CacheMode::NoStore
                && self.mode != CacheMode::Reload;
        if !is_cacheable {
            return Ok(Action::Remote(Fetch::Normal));
        }
        let method = request_parts.method.to_string().to_uppercase();
        let url = Url::parse(&request_parts.uri.to_string())?;
        if let Some(store) = self.manager.get(&method, &url).await? {
            let (mut response, policy) = store;
            response.cache_lookup_status(HitOrMiss::HIT);
            if let Some(warning_code) = response.warning_code() {
                // https://tools.ietf.org/html/rfc7234#section-4.3.4
                //
                // If a stored response is selected for update, the cache MUST:
                //
                // * delete any warning header fields in the stored response with
                //   warn-code 1xx (see Section 5.5);
                //
                // * retain any warning header fields in the stored response with
                //   warn-code 2xx;
                //
                if (100..200).contains(&warning_code) {
                    response.remove_warning();
                }
            }

            match self.mode {
                CacheMode::Default => {
                    Ok(Action::Remote(Fetch::Conditional(Stage::BeforeFetch {
                        response: Box::new(response),
                        policy: Box::new(policy),
                    })))
                }
                CacheMode::NoCache => Ok(Action::Remote(Fetch::ForceNoCache)),
                CacheMode::ForceCache | CacheMode::OnlyIfCached => {
                    //   112 Disconnected operation
                    // SHOULD be included if the cache is intentionally disconnected from
                    // the rest of the network for a period of time.
                    // (https://tools.ietf.org/html/rfc2616#section-14.46)
                    response.add_warning(
                        &response.url.clone(),
                        112,
                        "Disconnected operation",
                    );
                    response.cache_status(HitOrMiss::HIT);
                    Ok(Action::Cached { response: Box::new(response) })
                }
                _ => Ok(Action::Remote(Fetch::Normal)),
            }
        } else {
            match self.mode {
                CacheMode::OnlyIfCached => {
                    // ENOTCACHED
                    Ok(Action::Cached {
                        response: Box::new(HttpResponse {
                            body: b"GatewayTimeout".to_vec(),
                            status: 504,
                            url,
                            ..Default::default()
                        }),
                    })
                }
                _ => Ok(Action::Remote(Fetch::Normal)),
            }
        }
    }

    /// Runs after a remote fetch and determines what caching actions to take (if any).
    pub async fn after_remote_fetch(
        &self,
        response: &HttpResponse,
        request_parts: &request::Parts,
    ) -> Result<()> {
        let policy = self.build_policy(request_parts, response)?;
        let is_get_head =
            request_parts.method == "GET" || request_parts.method == "HEAD";
        let is_cacheable = is_get_head
            && self.mode != CacheMode::NoStore
            && self.mode != CacheMode::Reload
            && response.status == 200
            && policy.is_storable();
        let url = Url::parse(&request_parts.uri.to_string())?;
        let method = request_parts.method.to_string().to_uppercase();
        if is_cacheable {
            self.manager.put(&method, &url, response.clone(), policy).await?;
        } else if !is_get_head {
            self.manager.delete("GET", &url).await.ok();
        }
        Ok(())
    }

    /// Runs before a conditional fetch to determine freshness of the cached response.
    /// If the response is fresh, the conditional fetch logic will process immediately.
    /// If the response is not fresh, the conditional fetch logic is to continue after the request headers are updated.
    pub fn before_conditional_fetch(
        &self,
        request_parts: &request::Parts,
        mut cached_response: HttpResponse,
        policy: CachePolicy,
    ) -> Result<Stage> {
        let before_req =
            policy.before_request(request_parts, SystemTime::now());
        match before_req {
            BeforeRequest::Fresh(parts) => {
                cached_response.update_headers(&parts)?;
                cached_response.cache_status(HitOrMiss::HIT);
                cached_response.cache_lookup_status(HitOrMiss::HIT);
                Ok(Stage::Cached { response: Box::new(cached_response) })
            }
            BeforeRequest::Stale { request: parts, matches: _ } => {
                Ok(Stage::UpdateRequestHeaders {
                    request_parts: Box::new(parts),
                })
            }
        }
    }

    /// Runs after a conditional fetch and determines what caching actions to take (if any).
    pub async fn after_conditional_fetch(
        &self,
        request_parts: &request::Parts,
        mut cached_response: HttpResponse,
        mut conditional_response: HttpResponse,
        mut policy: CachePolicy,
    ) -> Result<HttpResponse> {
        cached_response.cache_lookup_status(HitOrMiss::HIT);
        conditional_response.cache_lookup_status(HitOrMiss::HIT);
        let url = Url::parse(&request_parts.uri.to_string())?;
        let method = request_parts.method.to_string().to_uppercase();
        let status = StatusCode::from_u16(conditional_response.status)?;
        if status.is_server_error() && cached_response.must_revalidate() {
            //   111 Revalidation failed
            //   MUST be included if a cache returns a stale response
            //   because an attempt to revalidate the response failed,
            //   due to an inability to reach the server.
            // (https://tools.ietf.org/html/rfc2616#section-14.46)
            cached_response.add_warning(&url, 111, "Revalidation failed");
            cached_response.cache_status(HitOrMiss::HIT);
            Ok(cached_response)
        } else if conditional_response.status == 304 {
            let after_res = policy.after_response(
                request_parts,
                &conditional_response.parts()?,
                SystemTime::now(),
            );
            match after_res {
                AfterResponse::Modified(new_policy, parts)
                | AfterResponse::NotModified(new_policy, parts) => {
                    policy = new_policy;
                    cached_response.update_headers(&parts)?;
                }
            }
            cached_response.cache_status(HitOrMiss::HIT);
            self.manager
                .put(&method, &url, cached_response.clone(), policy)
                .await?;
            Ok(cached_response.clone())
        } else if conditional_response.status == 200 {
            let policy =
                self.build_policy(request_parts, &conditional_response)?;
            conditional_response.cache_status(HitOrMiss::MISS);
            self.manager
                .put(&method, &url, conditional_response.clone(), policy)
                .await?;
            Ok(conditional_response)
        } else {
            cached_response.cache_status(HitOrMiss::HIT);
            Ok(cached_response)
        }
    }
}
