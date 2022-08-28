use crate::*;
use std::sync::Arc;

use http::{Request, Response};
use http_cache_tower::{Cache, CacheLayer};
use tower::{Service, ServiceBuilder, ServiceExt};

#[tokio::test]
async fn default_mode() -> Result<()> {
    let mock_server = MockServer::start().await;
    let m = build_mock(CACHEABLE_PUBLIC, TEST_BODY, 200, 1);
    let _mock_guard = mock_server.register_as_scoped(m).await;
    let url = format!("{}/", &mock_server.uri());
    let manager = MokaManager::default();

    // Construct tower service with hyper client and cache defaults
    let svc = ServiceBuilder::new()
        .layer(CacheLayer::new(HttpCache {
            mode: CacheMode::Default,
            manager: manager.clone(),
            options: None,
        }))
        .service(hyper::Client::new());

    let req = Request::builder().uri(url).body(hyper::Body::empty())?;

    // TODO: implement actual cache tests after logic has been added

    let res = svc.oneshot(req).await.unwrap();
    let body = hyper::body::to_bytes(res.into_body()).await?;
    assert_eq!(body, TEST_BODY);
    Ok(())
}
