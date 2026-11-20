//! In-nginx request benchmarks (req/s through the live module).
//!
//! Boots the staged nginx with our `.so` once, then hammers it
//! over raw TCP: a CoreFilters HTML rewrite vs a same-size
//! plain-text passthrough (A/B in one run, so CPU governors
//! affect both equally). Gated on `PS_NGINX_BENCH=1` (like the
//! live compat suite) and a prebuilt release `.so`.
use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

/// Byte-equal page bodies keep the A/B honest (same framing cost).
fn page_fragment() -> Vec<u8> {
    br#"<!DOCTYPE html><html><head><title>Bench</title><link rel="stylesheet" href="a.css"><style>body { color: red; } /* tint */</style><script src="a.js"></script><script>function f() { return 1 + 2; }</script></head><body><div class="a" id="x"><p>Hello <b>world</b> <!-- note --> <a href="/p?q=1">link</a></p><img src="i.png" width="10" height="10"></div></body></html>"#
        .to_vec()
}

/// Booted nginx under a scratch prefix (killed on drop).
struct Nginx {
    child: Child,
    port: u16,
    prefix: PathBuf,
}

impl Nginx {
    /// Boots the staged tree with our release module.
    fn boot() -> Option<Self> {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let root = manifest.parent()?.parent()?.to_owned();
        let binary = root.parent()?.join(".nginx-build/nginx-1.31.8/objs/nginx");
        let module_so = root.join("target/release/libps_nginx.so");
        let mime_types = root
            .parent()?
            .join(".nginx-build/nginx-1.31.8/conf/mime.types");
        if !binary.is_file() || !module_so.is_file() {
            eprintln!("skip: build the staged nginx and release .so first");
            return None;
        }
        let prefix = std::env::temp_dir().join(format!("nginx-bench-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&prefix);
        for dir in ["conf", "html", "cache", "logs"] {
            std::fs::create_dir_all(prefix.join(dir)).ok()?;
        }
        let mut page = Vec::new();
        for _ in 0..30 {
            page.extend_from_slice(&page_fragment());
        }
        std::fs::write(prefix.join("html/page.html"), &page).ok()?;
        std::fs::write(prefix.join("html/plain.txt"), &page).ok()?;
        std::fs::write(prefix.join("html/a.css"), b".a { color: red; }\n").ok()?;
        std::fs::write(prefix.join("html/a.js"), b"function f( ) { return 2 ; }\n").ok()?;
        // 1x1 opaque PNG (valid, tiny).
        let png = tiny_png();
        debug_assert!(ps_image::png::decode(&png).is_ok());
        std::fs::write(prefix.join("html/i.png"), png).ok()?;
        let port = free_port()?;
        let conf = format!(
            "load_module {};\n\
             worker_processes 1;\n\
             error_log logs/error.log;\n\
             pid logs/nginx.pid;\n\
             daemon off;\n\
             events {{ worker_connections 64; }}\n\
             http {{\n\
             include {};\n\
             access_log off;\n\
             pagespeed on;\n\
             pagespeed FileCachePath {};\n\
             pagespeed RewriteLevel CoreFilters;\n\
             server {{\n\
             listen {};\n\
             server_name 127.0.0.1 localhost;\n\
             root {};\n\
             location / {{ try_files $uri $uri/ =404; }}\n\
             }}\n\
             }}\n",
            module_so.display(),
            mime_types.display(),
            prefix.join("cache").display(),
            port,
            prefix.join("html").display(),
        );
        std::fs::write(prefix.join("conf/nginx.conf"), conf).ok()?;
        let child = Command::new(&binary)
            .arg("-p")
            .arg(&prefix)
            .arg("-c")
            .arg(prefix.join("conf/nginx.conf"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;
        let this = Nginx {
            child,
            port,
            prefix,
        };
        // Wait for the port (bounded).
        for _ in 0..100 {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                // Warm the caches (first hits compile metadata).
                for _ in 0..3 {
                    this.get("/page.html");
                    this.get("/plain.txt");
                }
                return Some(this);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }

    /// One raw HTTP/1.0 GET (read to EOF).
    fn get(&self, path: &str) -> Vec<u8> {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).expect("connect");
        stream
            .write_all(format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").as_bytes())
            .expect("write");
        let mut out = Vec::new();
        stream.read_to_end(&mut out).expect("read");
        assert!(
            out.starts_with(b"HTTP/1.1 200") || out.starts_with(b"HTTP/1.0 200"),
            "unexpected status: {}",
            String::from_utf8_lossy(&out[..out.len().min(60)])
        );
        out
    }
}

impl Drop for Nginx {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.prefix);
    }
}

/// Finds a free loopback port.
fn free_port() -> Option<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").ok()?;
    let port = listener.local_addr().ok()?.port();
    drop(listener);
    Some(port)
}

/// One red pixel PNG (opaque RGB).
fn tiny_png() -> Vec<u8> {
    // Minimal encoder-free bytes: signature, IHDR (1x1 RGB),
    // IDAT (stored deflate block), IEND, with stock CRCs.
    let mut out = vec![
        0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, b'I', b'H', b'D', b'R', 0, 0,
        0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0,
    ];
    let crc = crc32(&out[12..29]);
    out.extend_from_slice(&crc.to_be_bytes());
    // IDAT: zlib header + stored block (filter 0 + RGB ff0000) + adler32.
    let mut idat = vec![
        0x78, 0x01, 0x01, 0x04, 0x00, 0xfb, 0xff, 0x00, 0xff, 0x00, 0x00,
    ];
    let adler = adler32(&[0x00, 0xff, 0x00, 0x00]);
    idat.extend_from_slice(&adler.to_be_bytes());
    out.extend_from_slice(&(idat.len() as u32).to_be_bytes());
    out.extend_from_slice(b"IDAT");
    out.extend_from_slice(&idat);
    let start = out.len() - idat.len() - 4;
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(b"IEND");
    let crc = crc32(b"IEND");
    out.extend_from_slice(&crc.to_be_bytes());
    out
}

/// IEEE CRC-32 (PNG chunks).
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = crc & 1;
            crc >>= 1;
            if mask != 0 {
                crc ^= 0xedb8_8320;
            }
        }
    }
    !crc
}

/// Adler-32 (zlib trailer).
fn adler32(bytes: &[u8]) -> u32 {
    let mut low: u32 = 1;
    let mut high: u32 = 0;
    for &byte in bytes {
        low = (low + byte as u32) % 65521;
        high = (high + low) % 65521;
    }
    (high << 16) | low
}

fn bench_nginx(criterion: &mut Criterion) {
    if std::env::var("PS_NGINX_BENCH").as_deref() != Ok("1") {
        eprintln!("skipped: set PS_NGINX_BENCH=1");
        return;
    }
    let Some(nginx) = Nginx::boot() else {
        return;
    };
    let mut group = criterion.benchmark_group("nginx");
    group.bench_function("rewrite-html", |bencher| {
        bencher.iter(|| black_box(nginx.get("/page.html")));
    });
    group.bench_function("passthrough-text", |bencher| {
        bencher.iter(|| black_box(nginx.get("/plain.txt")));
    });
    group.finish();
}

criterion_group!(benches, bench_nginx);
criterion_main!(benches);
