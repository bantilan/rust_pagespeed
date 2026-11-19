//! Benchmarks for hashing, naming, and backends (Step 7 gate).
use criterion::{criterion_group, criterion_main, Criterion};
use ps_cache::cache::{Cache, FileCache, FileCachePolicy, LruCache, MemoryCache};
use ps_cache::hash::{Hasher, Md5Hasher};
use ps_cache::namer::ResourceNamer;
use std::hint::black_box;

fn bench_hasher(criterion: &mut Criterion) {
    let hasher = Md5Hasher::new();
    let body = vec![0x41u8; 4096];
    criterion.bench_function("md5_hash_4k", |bencher| {
        bencher.iter(|| black_box(hasher.hash(black_box(&body))));
    });
    criterion.bench_function("md5_hash_to_u64", |bencher| {
        bencher.iter(|| black_box(hasher.hash_to_u64(black_box(&body))));
    });
}

fn bench_namer(criterion: &mut Criterion) {
    criterion.bench_function("namer_encode", |bencher| {
        let mut namer = ResourceNamer::new();
        namer.set_name("style.css");
        namer.set_id("cf");
        namer.set_hash("1B2M2Y8Asg");
        namer.set_ext("css");
        bencher.iter(|| black_box(namer.encode()));
    });
    criterion.bench_function("namer_decode", |bencher| {
        bencher.iter(|| {
            let mut namer = ResourceNamer::new();
            assert!(namer.decode_ignore_hash_and_signature(black_box(
                "style.css.pagespeed.cf.1B2M2Y8Asg.css"
            )));
            black_box(namer)
        });
    });
}

fn bench_backends(criterion: &mut Criterion) {
    criterion.bench_function("memory_put_get", |bencher| {
        let cache = MemoryCache::new();
        let value = vec![7u8; 1024];
        bencher.iter(|| {
            cache.put(black_box("k"), black_box(&value));
            black_box(cache.get("k"))
        });
    });
    criterion.bench_function("lru_put_get", |bencher| {
        let cache = LruCache::new(10 << 20);
        let value = vec![7u8; 1024];
        bencher.iter(|| {
            cache.put(black_box("k"), black_box(&value));
            black_box(cache.get("k"))
        });
    });
    criterion.bench_function("file_put_get", |bencher| {
        let dir = std::env::temp_dir().join(format!("ps_cache_bench_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cache = FileCache::new(
            &dir,
            FileCachePolicy {
                clean_interval_ms: ps_cache::cache::DISABLE_CLEANING,
                target_size_bytes: 100 << 20,
                target_inode_count: 0,
                path_length_limit: ps_cache::cache::DEFAULT_PATH_LENGTH_LIMIT,
            },
            0,
        );
        let value = vec![7u8; 1024];
        bencher.iter(|| {
            cache.put(black_box("http://x/k"), black_box(&value));
            black_box(cache.get("http://x/k"))
        });
        let _ = std::fs::remove_dir_all(&dir);
    });
}

fn bench_remote(criterion: &mut Criterion) {
    use ps_cache::remote::{
        ClusterSpec, FakeTransport, MemcachedCache, RemoteTransport, MEMCACHED_DEFAULT_PORT,
    };
    use std::sync::Arc;
    use std::time::Duration;
    criterion.bench_function("memcached_hash_encode", |bencher| {
        let cluster = ClusterSpec::parse("127.0.0.1:11211", MEMCACHED_DEFAULT_PORT).unwrap();
        let cache = MemcachedCache::new(&cluster, Duration::from_secs(5));
        let _ = &cache; // live path needs a server; bench hashing instead
        let value = vec![7u8; 1024];
        bencher.iter(|| {
            black_box(cache.hashed_key(black_box("http://x/a.png")));
            black_box(ps_cache::cache::key_value_codec::encode(
                "http://x/a.png",
                black_box(&value),
            ))
        });
    });
    criterion.bench_function("fake_transport_put_get", |bencher| {
        let transport = Arc::new(FakeTransport::new());
        let value = vec![7u8; 1024];
        bencher.iter(|| {
            transport
                .transport_set(black_box("k"), black_box(&value), None)
                .unwrap();
            black_box(transport.transport_get("k").unwrap())
        });
    });
}

fn bench_html(criterion: &mut Criterion) {
    criterion.bench_function("html_key_sign", |bencher| {
        bencher.iter(|| {
            black_box(ps_cache::html::html_cache_key(black_box(
                "http://x/\n\nsig\nreq\n",
            )))
        });
    });
    criterion.bench_function("purge_is_valid", |bencher| {
        let set = ps_cache::purge::PurgeSet::new(25_000);
        bencher.iter(|| black_box(set.is_valid(black_box("http://x/a"), black_box(1000))));
    });
}

criterion_group!(
    benches,
    bench_hasher,
    bench_namer,
    bench_backends,
    bench_remote,
    bench_html
);
criterion_main!(benches);
