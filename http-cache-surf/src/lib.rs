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
//! The surf middleware implementation for http-cache.
//! ```no_run
//! use http_cache_surf::{Cache, CacheMode, CACacheManager, HttpCache};
//!
//! #[async_std::main]
//! async fn main() -> surf::Result<()> {
//!     let req = surf::get("https://developer.mozilla.org/en-US/docs/Web/HTTP/Caching");
//!     surf::client()
//!         .with(Cache(HttpCache {
//!             mode: CacheMode::Default,
//!             manager: CACacheManager::default(),
//!             options: None,
//!         }))
//!         .send(req)
//!         .await?;
//!     Ok(())
//! }
//! ```
mod error;

use anyhow::anyhow;
use std::{convert::TryInto, str::FromStr};

use http::{header::CACHE_CONTROL, request};
use http_cache::{
    Action, BadHeader, CacheManager, Fetch, HitOrMiss, Result, Stage,
};
use http_types::{headers::HeaderValue, Response, StatusCode, Version};
use surf::{middleware::Next, Client, Request};
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

async fn convert_from_http_types_response(
    mut response: Response,
    url: Url,
) -> Result<HttpResponse> {
    let mut converted = HttpResponse::default();
    for header in response.iter() {
        converted
            .headers
            .insert(header.0.as_str().to_owned(), header.1.as_str().to_owned());
    }
    converted.url = url;
    converted.status = response.status().into();
    converted.version =
        response.version().unwrap_or(Version::Http1_1).try_into()?;
    let body: Vec<u8> = match response.body_bytes().await {
        Ok(b) => b,
        Err(e) => return Err(Box::new(error::Error::Surf(anyhow!(e)))),
    };
    converted.body = body;
    Ok(converted)
}

fn convert_to_http_types_response(
    response: HttpResponse,
) -> std::result::Result<Response, http_types::Error> {
    let mut converted = Response::new(StatusCode::Ok);
    for header in &response.headers {
        let val = HeaderValue::from_bytes(header.1.as_bytes().to_vec())?;
        converted.insert_header(header.0.as_str(), val);
    }
    converted.set_status(response.status.try_into()?);
    converted.set_version(Some(response.version.try_into()?));
    converted.set_body(response.body);
    Ok(converted)
}

#[surf::utils::async_trait]
impl<T: CacheManager + Send + Sync + 'static> surf::middleware::Middleware
    for Cache<T>
{
    async fn handle(
        &self,
        mut req: Request,
        client: Client,
        next: Next<'_>,
    ) -> std::result::Result<surf::Response, http_types::Error> {
        let mut converted = request::Builder::new()
            .method(req.method().as_ref())
            .uri(req.url().as_str())
            .body(())?;
        {
            let headers = converted.headers_mut();
            for header in req.iter() {
                headers.insert(
                    http::header::HeaderName::from_str(header.0.as_str())?,
                    http::HeaderValue::from_str(header.1.as_str())?,
                );
            }
        }
        let request_parts = converted.into_parts().0;
        let url = req.url().clone();
        match self.0.before_request(&request_parts).await {
            Ok(action) => match action {
                Action::Cached { response } => {
                    let converted = convert_to_http_types_response(*response)?;
                    Ok(surf::Response::from(converted))
                }
                Action::Remote(fetch) => match fetch {
                    Fetch::Normal => {
                        let res = next.run(req, client).await?;
                        let mut response =
                            match convert_from_http_types_response(
                                res.into(),
                                url,
                            )
                            .await
                            {
                                Ok(r) => r,
                                Err(e) => {
                                    return Err(http_types::Error::from(
                                        anyhow!(e),
                                    ))
                                }
                            };
                        match self
                            .0
                            .after_remote_fetch(&mut response, &request_parts)
                            .await
                        {
                            Ok(r) => r,
                            Err(e) => {
                                return Err(http_types::Error::from(anyhow!(e)))
                            }
                        };
                        let converted =
                            convert_to_http_types_response(response)?;
                        return Ok(surf::Response::from(converted));
                    }
                    Fetch::ForceNoCache => {
                        req.insert_header(CACHE_CONTROL.as_str(), "no-cache");
                        let res = next.run(req, client).await?;
                        let mut response =
                            match convert_from_http_types_response(
                                res.into(),
                                url,
                            )
                            .await
                            {
                                Ok(r) => r,
                                Err(e) => {
                                    return Err(http_types::Error::from(
                                        anyhow!(e),
                                    ))
                                }
                            };
                        match self
                            .0
                            .after_remote_fetch(&mut response, &request_parts)
                            .await
                        {
                            Ok(r) => r,
                            Err(e) => {
                                return Err(http_types::Error::from(anyhow!(e)))
                            }
                        };
                        response.cache_lookup_status(HitOrMiss::HIT);
                        let converted =
                            convert_to_http_types_response(response)?;
                        return Ok(surf::Response::from(converted));
                    }
                    Fetch::Conditional(stage) => match stage {
                        Stage::BeforeFetch { response, policy } => {
                            match self.0.before_conditional_fetch(
                                &request_parts,
                                *response.clone(),
                                *policy.clone(),
                            ) {
                                Ok(stage) => {
                                    match stage {
                                        Stage::Cached { response } => {
                                            let converted =
                                                convert_to_http_types_response(
                                                    *response,
                                                )?;
                                            return Ok(surf::Response::from(
                                                converted,
                                            ));
                                        }
                                        Stage::UpdateRequestHeaders {
                                            request_parts,
                                        } => {
                                            for header in
                                                request_parts.headers.iter()
                                            {
                                                let value = match HeaderValue::from_str(header.1.to_str()?) {
                                                    Ok(v) => v,
                                                    Err(_e) => return Err(http_types::Error::from(BadHeader)),
                                                };
                                                req.set_header(
                                                    header.0.as_str(),
                                                    value,
                                                );
                                            }
                                            let res =
                                                next.run(req, client).await?;
                                            let conditional_response = match convert_from_http_types_response(res.into(), url).await {
                                                    Ok(r) => r,
                                                    Err(e) => return Err(http_types::Error::from(anyhow!(e))),
                                                };
                                            let response =
                                                match self
                                                    .0
                                                    .after_conditional_fetch(
                                                        &request_parts,
                                                        *response.clone(),
                                                        conditional_response,
                                                        *policy,
                                                    )
                                                    .await
                                                {
                                                    Ok(r) => r,
                                                    Err(e) => return Err(
                                                        http_types::Error::from(
                                                            anyhow!(e),
                                                        ),
                                                    ),
                                                };
                                            let converted =
                                                convert_to_http_types_response(
                                                    response,
                                                )?;
                                            return Ok(surf::Response::from(
                                                converted,
                                            ));
                                        }
                                        Stage::BeforeFetch {
                                            response: _,
                                            policy: _,
                                        } => unreachable!(),
                                    }
                                }
                                Err(e) => {
                                    return Err(http_types::Error::from(
                                        anyhow!(e),
                                    ))
                                }
                            }
                        }
                        Stage::Cached { response: _ } => unreachable!(),
                        Stage::UpdateRequestHeaders { request_parts: _ } => {
                            unreachable!()
                        }
                    },
                },
            },
            Err(e) => return Err(http_types::Error::from(anyhow!(e))),
        }
    }
}
