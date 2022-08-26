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
use http_cache::{Action, CacheManager, Fetch, HitOrMiss, Result, Stage};
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

// #[async_trait::async_trait]
// impl Middleware for ReqwestMiddleware<'_> {
//     fn is_method_get_head(&self) -> bool {
//         self.req.method() == Method::GET || self.req.method() == Method::HEAD
//     }
//     fn policy(&self, response: &HttpResponse) -> Result<CachePolicy> {
//         Ok(CachePolicy::new(&self.parts()?, &response.parts()?))
//     }
//     fn policy_with_options(
//         &self,
//         response: &HttpResponse,
//         options: CacheOptions,
//     ) -> Result<CachePolicy> {
//         Ok(CachePolicy::new_options(
//             &self.parts()?,
//             &response.parts()?,
//             SystemTime::now(),
//             options,
//         ))
//     }
//     fn update_headers(&mut self, parts: &Parts) -> Result<()> {
//         for header in parts.headers.iter() {
//             self.req.headers_mut().insert(header.0.clone(), header.1.clone());
//         }
//         Ok(())
//     }
//     fn force_no_cache(&mut self) -> Result<()> {
//         self.req
//             .headers_mut()
//             .insert(CACHE_CONTROL, HeaderValue::from_str("no-cache")?);
//         Ok(())
//     }
//     fn parts(&self) -> Result<Parts> {
//         let copied_req =
//             self.req.try_clone().ok_or_else(|| Box::new(BadRequest))?;
//         let converted = match http::Request::try_from(copied_req) {
//             Ok(r) => r,
//             Err(e) => return Err(Box::new(e)),
//         };
//         Ok(converted.into_parts().0)
//     }
//     fn url(&self) -> Result<Url> {
//         Ok(self.req.url().clone())
//     }
//     fn method(&self) -> Result<String> {
//         Ok(self.req.method().as_ref().to_string())
//     }
//     async fn remote_fetch(&mut self) -> Result<HttpResponse> {
//         let copied_req =
//             self.req.try_clone().ok_or_else(|| Box::new(BadRequest))?;
//         let res = match self.next.clone().run(copied_req, self.extensions).await
//         {
//             Ok(r) => r,
//             Err(e) => return Err(Box::new(e)),
//         };
//         let mut headers = HashMap::new();
//         for header in res.headers() {
//             headers.insert(
//                 header.0.as_str().to_owned(),
//                 header.1.to_str()?.to_owned(),
//             );
//         }
//         let url = res.url().clone();
//         let status = res.status().into();
//         let version = res.version();
//         let body: Vec<u8> = match res.bytes().await {
//             Ok(b) => b,
//             Err(e) => return Err(Box::new(e)),
//         }
//         .to_vec();
//         Ok(HttpResponse {
//             body,
//             headers,
//             status,
//             url,
//             version: version.try_into()?,
//         })
//     }
// }

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
        let converted = match http::Request::try_from(copied_req) {
            Ok(r) => r,
            Err(e) => return Err(Error::Middleware(anyhow!(e))),
        };
        let request_parts = converted.into_parts().0;
        let url = req.url().clone();
        match self.0.before_request(&request_parts).await {
            Ok(action) => match action {
                Action::Cached { response } => {
                    let converted = convert_to_reqwest_response(*response)?;
                    Ok(converted)
                }
                Action::Remote(fetch) => match fetch {
                    Fetch::Normal => {
                        let copied_req = clone_req(&req)?;
                        let res = next.run(copied_req, extensions).await?;
                        let mut response =
                            match convert_from_reqwest_response(res, url).await
                            {
                                Ok(r) => r,
                                Err(e) => {
                                    return Err(Error::Middleware(anyhow!(e)))
                                }
                            };
                        match self
                            .0
                            .after_remote_fetch(&mut response, &request_parts)
                            .await
                        {
                            Ok(r) => r,
                            Err(e) => {
                                return Err(Error::Middleware(anyhow!(e)))
                            }
                        };
                        let converted = convert_to_reqwest_response(response)?;
                        return Ok(converted);
                    }
                    Fetch::ForceNoCache => {
                        let v = match HeaderValue::from_str("no-cache") {
                            Ok(v) => v,
                            Err(e) => {
                                return Err(Error::Middleware(anyhow!(e)))
                            }
                        };
                        req.headers_mut().insert(CACHE_CONTROL.as_str(), v);
                        let res = next.run(req, extensions).await?;
                        let mut response =
                            match convert_from_reqwest_response(res, url).await
                            {
                                Ok(r) => r,
                                Err(e) => {
                                    return Err(Error::Middleware(anyhow!(e)))
                                }
                            };
                        match self
                            .0
                            .after_remote_fetch(&mut response, &request_parts)
                            .await
                        {
                            Ok(r) => r,
                            Err(e) => {
                                return Err(Error::Middleware(anyhow!(e)))
                            }
                        };
                        response.cache_lookup_status(HitOrMiss::HIT);
                        let converted = convert_to_reqwest_response(response)?;
                        return Ok(converted);
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
                                                convert_to_reqwest_response(
                                                    *response,
                                                )?;
                                            return Ok(converted);
                                        }
                                        Stage::UpdateRequestHeaders {
                                            request_parts,
                                        } => {
                                            for header in
                                                request_parts.headers.iter()
                                            {
                                                req.headers_mut().insert(
                                                    header.0.clone(),
                                                    HeaderValue::from(header.1),
                                                );
                                            }
                                            let res = next
                                                .run(req, extensions)
                                                .await?;
                                            let conditional_response = match convert_from_reqwest_response(res, url).await {
                                                    Ok(r) => r,
                                                    Err(e) => return Err(Error::Middleware(anyhow!(e))),
                                                };
                                            let response = match self
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
                                                Err(e) => {
                                                    return Err(
                                                        Error::Middleware(
                                                            anyhow!(e),
                                                        ),
                                                    )
                                                }
                                            };
                                            let converted =
                                                convert_to_reqwest_response(
                                                    response,
                                                )?;
                                            return Ok(converted);
                                        }
                                        Stage::BeforeFetch {
                                            response: _,
                                            policy: _,
                                        } => unreachable!(),
                                    }
                                }
                                Err(e) => {
                                    return Err(Error::Middleware(anyhow!(e)))
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
            Err(e) => return Err(Error::Middleware(anyhow!(e))),
        }
    }
}
