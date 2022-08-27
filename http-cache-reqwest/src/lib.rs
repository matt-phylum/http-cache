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
//! The reqwest middleware implementation for http-cache.
//! ```no_run
//! use reqwest::Client;
//! use reqwest_middleware::{ClientBuilder, Result};
//! use http_cache_reqwest::{Cache, CacheMode, CACacheManager, HttpCache};
//!
//! #[tokio::main]
//! async fn main() -> Result<()> {
//!     let client = ClientBuilder::new(Client::new())
//!         .with(Cache(HttpCache {
//!             mode: CacheMode::Default,
//!             manager: CACacheManager::default(),
//!             options: None,
//!         }))
//!         .build();
//!     client
//!         .get("https://developer.mozilla.org/en-US/docs/Web/HTTP/Caching")
//!         .send()
//!         .await?;
//!     Ok(())
//! }
//! ```
mod error;

use anyhow::anyhow;

pub use error::BadRequest;
use std::{convert::TryInto, str::FromStr};

use http::{
    header::{HeaderName, CACHE_CONTROL},
    HeaderValue,
};
use http_cache::{Action, BoxError, CacheManager, Fetch, Result, Stage};
use reqwest::{Request, Response, ResponseBuilderExt};
use reqwest_middleware::{Error, Next};
use task_local_extensions::Extensions;
use url::Url;

pub use http_cache::{CacheMode, CacheOptions, HttpCache, HttpResponse};

#[cfg(feature = "manager-cacache")]
#[cfg_attr(docsrs, doc(cfg(feature = "manager-cacache")))]
pub use http_cache::CACacheManager;

#[cfg(feature = "manager-moka")]
#[cfg_attr(docsrs, doc(cfg(feature = "manager-moka")))]
pub use http_cache::{MokaCache, MokaCacheBuilder, MokaManager};

/// Wrapper for [`HttpCache`]
#[derive(Debug)]
pub struct Cache<T: CacheManager>(pub HttpCache<T>);

// Converts an [`HttpResponse`] to a reqwest [`Response`]
fn convert_to_reqwest_response(
    response: HttpResponse,
) -> anyhow::Result<Response> {
    let mut res = http::Response::builder()
        .status(response.status)
        .url(response.url)
        .version(response.version.try_into()?)
        .body(response.body)?;
    for header in response.headers {
        res.headers_mut().insert(
            HeaderName::from_str(header.0.clone().as_str())?,
            HeaderValue::from_str(header.1.clone().as_str())?,
        );
    }
    Ok(Response::from(res))
}

// Converts a reqwest [`Response`] to an [`HttpResponse`]
async fn convert_from_reqwest_response(
    response: Response,
    url: Url,
) -> Result<HttpResponse> {
    let mut converted = HttpResponse::default();
    for header in response.headers() {
        converted.headers.insert(
            header.0.as_str().to_owned(),
            header.1.to_str()?.to_owned(),
        );
    }
    converted.url = url;
    converted.status = response.status().into();
    converted.version = response.version().try_into()?;
    let body: Vec<u8> = match response.bytes().await {
        Ok(b) => b.to_vec(),
        Err(e) => return Err(Box::new(e)),
    };
    converted.body = body;
    Ok(converted)
}

fn clone_req(request: &Request) -> std::result::Result<Request, Error> {
    match request.try_clone() {
        Some(r) => Ok(r),
        None => Err(Error::Middleware(anyhow!(BadRequest))),
    }
}

fn to_middleware_error(e: BoxError) -> Error {
    Error::Middleware(anyhow!(e))
}

#[async_trait::async_trait]
impl<T: CacheManager + Send + Sync + 'static> reqwest_middleware::Middleware
    for Cache<T>
{
    async fn handle(
        &self,
        mut req: Request,
        extensions: &mut Extensions,
        next: Next<'_>,
    ) -> std::result::Result<Response, Error> {
        let copied_req = clone_req(&req)?;
        let converted = http::Request::try_from(copied_req)?;
        let request_parts = converted.into_parts().0;
        let url = req.url().clone();
        let action = self
            .0
            .before_request(&request_parts)
            .await
            .map_err(to_middleware_error)?;
        match action {
            Action::Cached { response } => {
                let converted = convert_to_reqwest_response(*response)?;
                Ok(converted)
            }
            Action::Remote(fetch) => match fetch {
                Fetch::Normal => {
                    let copied_req = clone_req(&req)?;
                    let res = next.run(copied_req, extensions).await?;
                    let response = convert_from_reqwest_response(res, url)
                        .await
                        .map_err(to_middleware_error)?;
                    self.0
                        .after_remote_fetch(&response, &request_parts)
                        .await
                        .map_err(to_middleware_error)?;
                    let converted = convert_to_reqwest_response(response)?;
                    return Ok(converted);
                }
                Fetch::ForceNoCache => {
                    req.headers_mut().insert(
                        CACHE_CONTROL.as_str(),
                        HeaderValue::from_str("no-cache")
                            .map_err(|e| to_middleware_error(Box::new(e)))?,
                    );
                    let res = next.run(req, extensions).await?;
                    let mut response = convert_from_reqwest_response(res, url)
                        .await
                        .map_err(to_middleware_error)?;
                    self.0
                        .after_remote_fetch(&response, &request_parts)
                        .await
                        .map_err(to_middleware_error)?;
                    response.cache_lookup_status(http_cache::HitOrMiss::HIT);
                    let converted = convert_to_reqwest_response(response)?;
                    return Ok(converted);
                }
                Fetch::Conditional(stage) => match stage {
                    Stage::BeforeFetch { response, policy } => {
                        let next_stage = self
                            .0
                            .before_conditional_fetch(
                                &request_parts,
                                *response.clone(),
                                *policy.clone(),
                            )
                            .map_err(to_middleware_error)?;
                        match next_stage {
                            Stage::Cached { response } => {
                                let converted =
                                    convert_to_reqwest_response(*response)?;
                                return Ok(converted);
                            }
                            Stage::UpdateRequestHeaders { request_parts } => {
                                for header in request_parts.headers.iter() {
                                    req.headers_mut().insert(
                                        header.0.clone(),
                                        HeaderValue::from(header.1),
                                    );
                                }
                                let res = next.run(req, extensions).await?;
                                let conditional_response =
                                    convert_from_reqwest_response(res, url)
                                        .await
                                        .map_err(to_middleware_error)?;
                                let response = self
                                    .0
                                    .after_conditional_fetch(
                                        &request_parts,
                                        *response.clone(),
                                        conditional_response,
                                        *policy,
                                    )
                                    .await
                                    .map_err(to_middleware_error)?;
                                let converted =
                                    convert_to_reqwest_response(response)?;
                                return Ok(converted);
                            }
                            Stage::BeforeFetch { response: _, policy: _ } => {
                                unreachable!()
                            }
                        }
                    }
                    Stage::Cached { response: _ } => unreachable!(),
                    Stage::UpdateRequestHeaders { request_parts: _ } => {
                        unreachable!()
                    }
                },
            },
        }
    }
}
