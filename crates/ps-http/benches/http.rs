//! Benchmarks for headers, parsing, and HTTP cache (Step 7 gate).
use criterion::{criterion_group, criterion_main, Criterion};
use ps_http::parser::{HeadersParser, ResponseParser};
use ps_http::response::{HttpOptions, ResponseHeaders};
use std::hint::black_box;

fn bench_parse(criterion: &mut Criterion) {
    let raw = b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nCache-Control: max-age=60\r\nContent-Length: 100\r\n\r\n";
    criterion.bench_function("headers_parse", |bencher| {
        bencher.iter(|| {
            let mut response = ResponseHeaders::new(HttpOptions::default_for_tests());
            let mut parser = HeadersParser::new(&mut response);
            black_box(parser.parse_chunk(black_box(raw)))
        });
    });
    criterion.bench_function("response_parse", |bencher| {
        bencher.iter(|| {
            let mut response = ResponseHeaders::new(HttpOptions::default_for_tests());
            let mut body = Vec::new();
            let mut parser = ResponseParser::new(&mut response, &mut body);
            black_box(parser.parse_chunk(black_box(raw)))
        });
    });
}

fn bench_caching(criterion: &mut Criterion) {
    criterion.bench_function("compute_caching", |bencher| {
        bencher.iter(|| {
            let mut response = ResponseHeaders::new(HttpOptions::default_for_tests());
            response.set_first_line(1, 1, 200, "OK");
            response
                .headers_mut()
                .add("Date", "Sun, 06 Nov 1994 08:49:37 GMT");
            response
                .headers_mut()
                .add("Cache-Control", "max-age=60, public");
            response.headers_mut().add("Content-Type", "image/png");
            response.compute_caching();
            black_box(response)
        });
    });
    criterion.bench_function("binary_round_trip", |bencher| {
        let mut response = ResponseHeaders::new(HttpOptions::default_for_tests());
        response.set_first_line(1, 1, 200, "OK");
        response
            .headers_mut()
            .add("Date", "Sun, 06 Nov 1994 08:49:37 GMT");
        response.headers_mut().add("Cache-Control", "max-age=60");
        response.compute_caching();
        let bytes = response.write_as_binary();
        bencher.iter(|| {
            let mut back = ResponseHeaders::new(HttpOptions::default_for_tests());
            back.read_from_binary(black_box(&bytes)).unwrap();
            black_box(back)
        });
    });
}

fn bench_cache(criterion: &mut Criterion) {
    use ps_cache::cache::MemoryCache;
    use ps_http::http_cache::HttpCache;
    use ps_http::request::RequestContext;
    use ps_http::response::{RequestProperties, VaryOption};
    use std::sync::Arc;
    criterion.bench_function("http_cache_put_find", |bencher| {
        let cache = HttpCache::new(Arc::new(MemoryCache::new()));
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        headers.set_first_line(1, 1, 200, "OK");
        headers.set_date(1_700_000_000_000);
        headers.headers_mut().add("Content-Type", "image/png");
        headers.headers_mut().add("Cache-Control", "max-age=60");
        let body = vec![7u8; 4096];
        let req = RequestProperties {
            has_cookie: false,
            has_cookie2: false,
            has_authorization: false,
        };
        bencher.iter(|| {
            cache.put(
                black_box("http://x/a.png"),
                "",
                req,
                VaryOption::RespectVaryOnResources,
                &mut headers.clone(),
                black_box(&body),
                1_700_000_000_000,
            );
            let mut context = RequestContext::new(HttpOptions::default_for_tests());
            black_box(cache.find(
                "http://x/a.png",
                "",
                req,
                VaryOption::RespectVaryOnResources,
                &|_| true,
                0,
                &mut context,
                1_700_000_000_000,
            ))
        });
    });
}

criterion_group!(benches, bench_parse, bench_caching, bench_cache);
criterion_main!(benches);
