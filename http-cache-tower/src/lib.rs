use futures_core::ready;
use futures_util::future::Either;

use std::{
    convert::TryFrom,
    convert::TryInto,
    future::Future,
    mem,
    pin::Pin,
    str,
    str::FromStr,
    task::{Context, Poll},
};

use http::{
    header::{HeaderName, CACHE_CONTROL},
    HeaderMap, HeaderValue, Method, Request, Response, StatusCode, Uri,
    Version, request,
};
use http_body::Body;
use http_cache::{Action, BoxError, CacheManager, Fetch, Result, Stage};
use pin_project_lite::pin_project;
use tower::util::Oneshot;
use tower_layer::Layer;
use tower_service::Service;

pub use http_cache::{CacheMode, CacheOptions, HttpCache, HttpResponse};

#[cfg(feature = "manager-cacache")]
#[cfg_attr(docsrs, doc(cfg(feature = "manager-cacache")))]
pub use http_cache::CACacheManager;

#[cfg(feature = "manager-moka")]
#[cfg_attr(docsrs, doc(cfg(feature = "manager-moka")))]
pub use http_cache::{MokaCache, MokaCacheBuilder, MokaManager};

/// Wrapper for [`HttpCache`]
#[derive(Debug)]
pub struct Cache<S, T: CacheManager> {
    inner: S,
    cache: HttpCache<T>,
}

impl<S, T> Cache<S, T>
where
    T: Clone + CacheManager,
{
    /// Create a new [`Cache`].
    pub fn new(inner: S, cache: HttpCache<T>) -> Self {
        Self { inner, cache }
    }

    /// Returns a new [`Layer`] that wraps services with a `Cache` middleware.
    pub fn layer(&self) -> CacheLayer<T> {
        CacheLayer::new(self.cache.clone())
    }
}

/// [`Layer`] with a [`Service`] to cache responses.
#[derive(Clone, Debug)]
pub struct CacheLayer<T: CacheManager> {
    cache: HttpCache<T>,
}

impl<T> CacheLayer<T>
where
    T: CacheManager,
{
    /// Create a new [`CacheLayer`].
    pub fn new(cache: HttpCache<T>) -> Self {
        Self { cache }
    }
}

impl<S, T> Layer<S> for CacheLayer<T>
where
    S: Clone,
    T: Clone + CacheManager,
{
    type Service = Cache<S, T>;

    fn layer(&self, inner: S) -> Self::Service {
        Cache { inner, cache: self.cache.clone() }
    }
}

impl<ReqBody, ResBody, S, T> Service<Request<ReqBody>> for Cache<S, T>
where
    S: Clone + Service<Request<ReqBody>, Response = Response<ResBody>>,
    ReqBody: Body + Default,
    T: Clone + CacheManager,
{
    type Response = Response<ResBody>;
    type Error = S::Error;
    type Future = ResponseFuture<S, ReqBody, T>;

    fn poll_ready(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        let service = self.inner.clone();
        let mut service = mem::replace(&mut self.inner, service);
        // TODO: figure out how to clone the body
        // TODO: figure out a way around cloning the cache if possible
        let body = ReqBody::default();
        ResponseFuture {
            method: req.method().clone(),
            uri: req.uri().clone(),
            version: req.version(),
            headers: req.headers().clone(),
            body,
            future: Either::Left(service.call(req)),
            service,
            cache: self.cache.clone(),
        }
    }
}

pin_project! {
    /// Response future for [`Cache`].
    #[derive(Debug)]
    pub struct ResponseFuture<S, B, T>
    where
        S: Service<Request<B>>,
        T: CacheManager,
    {
        #[pin]
        future: Either<S::Future, Oneshot<S, Request<B>>>,
        service: S,
        method: Method,
        uri: Uri,
        version: Version,
        headers: HeaderMap<HeaderValue>,
        body: B,
        cache: HttpCache<T>,
    }
}

impl<S, ReqBody, ResBody, T> Future for ResponseFuture<S, ReqBody, T>
where
    S: Service<Request<ReqBody>, Response = Response<ResBody>> + Clone,
    ReqBody: Body + Default,
    T: CacheManager + Clone,
{
    type Output = std::result::Result<Response<ResBody>, S::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();
        // TODO: figure out how to deal with all these unwraps
        let mut request = Request::builder()
            .method(this.method.clone())
            .uri(this.uri.clone())
            .version(*this.version)
            .body(()).unwrap();

        {
            let headers = request.headers_mut();
            for header in this.headers.iter() {
                headers.insert(
                    http::header::HeaderName::from_str(header.0.as_str()).unwrap(),
                    http::HeaderValue::from_str(header.1.to_str().unwrap()).unwrap(),
                );
            }
        }
        let request_parts = request.into_parts().0;
        // TODO: figure out how to run an async methods here
        // let action = this
        //     .cache
        //     .before_request(&request_parts)
        //     .await;
        let res = ready!(this.future.as_mut().poll(cx)?);
        Poll::Ready(Ok(res))
    }
}
