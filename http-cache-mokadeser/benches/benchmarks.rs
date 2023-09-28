use criterion::{
    async_executor::FuturesExecutor, criterion_group, criterion_main,
    BenchmarkId, Criterion,
};
use fake::Fake;
use http_cache::{CacheManager, HttpResponse, HttpVersion};
use http_cache_mokadeser::MokaManager;
use http_cache_semantics::CachePolicy;
use url::Url;

const URL: &str = "http://example.com";

async fn test_mokadeser(input: &str) {
    let url = Url::parse(URL).unwrap();
    let body = input.as_bytes().to_vec();
    let response = HttpResponse {
        body: body.clone(),
        headers: Default::default(),
        status: 200,
        url: url.clone(),
        version: HttpVersion::Http11,
    };
    let req = http::Request::get(URL).body(()).unwrap();
    let res = http::Response::builder().status(200).body(body).unwrap();
    let policy = CachePolicy::new(&req, &res);
    let manager = MokaManager::default();
    manager
        .put(format!("{}:{}", "GET", &url), response, policy.clone())
        .await
        .unwrap();
}

fn criterion_benchmark(c: &mut Criterion) {
    let str1 = ("200", 200.fake::<String>());
    let str2 = ("2000", 2000.fake::<String>());
    let str3 = ("20000", 20000.fake::<String>());
    let inputs = [("test", String::from("test")), str1, str2, str3];
    let mut group = c.benchmark_group("cache_managers");
    for i in inputs {
        group.bench_with_input(
            BenchmarkId::new("mokadeser", i.0),
            &i.1.as_str(),
            |b, &s| {
                // Insert a call to `to_async` to convert the bencher to async mode.
                // The timing loops are the same as with the normal bencher.
                b.to_async(FuturesExecutor).iter(|| test_mokadeser(s));
            },
        );
    }
    group.finish();
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
