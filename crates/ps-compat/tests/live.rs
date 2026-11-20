//! Live compat runners (Phase 4a): drive real nginx stacks.
//!
//! Spawns a staged nginx with either the Rust `.so` or the C++
//! module, serves one [`PageCase`][ps_compat::harness::PageCase]
//! per boot, and evaluates its assertions. Gated on
//! `PS_COMPAT_LIVE=1` (paths overridable; see below), so plain
//! `cargo test` stays hermetic.
//!
//! Knobs (all optional):
//! - `PS_COMPAT_LIVE=1`: enable (required).
//! - `PS_COMPAT_RUST_NGINX`: nginx binary for the Rust stack
//!   (default: the staged 1.31.8 tree's `objs/nginx`).
//! - `PS_COMPAT_CPP_NGINX`: nginx binary for the C++ stack
//!   (default: `/usr/sbin/nginx`).
//! - `PS_COMPAT_RUST_SO`: Rust module (default: the
//!   workspace `target/release/libps_nginx.so`, rebuilt via
//!   cargo when stale).
//! - `PS_COMPAT_CPP_SO`: C++ module (default:
//!   `/usr/lib/nginx/modules/ngx_pagespeed.so`).
//! - `PS_COMPAT_PORT_BASE`: first probe port (default 18090;
//!   Rust takes base, C++ base+1).

use ps_compat::harness::{evaluate, Assertion, AssertionResult, ConfigCase, PageCase, Runner};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

mod corpus;

/// Skips the test unless live mode is enabled.
fn live_enabled() -> bool {
    std::env::var("PS_COMPAT_LIVE").as_deref() == Ok("1")
}

/// Workspace root (this file lives in `crates/ps-compat/tests/`).
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates dir")
        .parent()
        .expect("workspace root")
        .to_owned()
}

/// Reads an env knob or its default.
fn knob(name: &str, default: String) -> String {
    std::env::var(name).unwrap_or(default)
}

/// One nginx instance: prefix dirs, config, master process.
struct NginxInstance {
    /// nginx binary.
    binary: String,
    /// Prefix directory (owned; removed on success).
    prefix: PathBuf,
    /// Listen port.
    port: u16,
    /// Master PID once started.
    master: Option<u32>,
    /// Whether the run failed (keeps the prefix for forensics).
    failed: bool,
}

impl NginxInstance {
    /// Creates a prefix with stock temp dirs.
    fn create(binary: String, tag: &str, port: u16) -> std::io::Result<Self> {
        let prefix = std::env::temp_dir().join(format!(
            "ps-compat-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));
        for dir in [
            "conf",
            "logs",
            "client_body_temp",
            "proxy_temp",
            "fastcgi_temp",
            "uwsgi_temp",
            "scgi_temp",
            "cache",
            "html",
        ] {
            std::fs::create_dir_all(prefix.join(dir))?;
        }
        Ok(NginxInstance {
            binary,
            prefix,
            port,
            master: None,
            failed: false,
        })
    }

    /// Writes `conf/nginx.conf` for one page case.
    fn write_case_config(
        &self,
        module_so: &str,
        case: &PageCase,
        doc_root: &Path,
    ) -> std::io::Result<()> {
        let mut directives = String::new();
        for (name, value) in &case.directives {
            let value = value.replace(
                corpus::CACHE_PLACEHOLDER,
                &self.prefix.join("cache").display().to_string(),
            );
            if value.is_empty() {
                directives.push_str(&format!("    pagespeed {name};\n"));
            } else {
                directives.push_str(&format!("    pagespeed {name} {value};\n"));
            }
        }
        // Staged tree's mime types (absolute, so both stacks
        // share it): without content types, static resources
        // serve text/plain and neither stack rewrites them.
        let mime_types = workspace_root()
            .parent()
            .expect("projects dir")
            .join(".nginx-build/nginx-1.31.8/conf/mime.types")
            .display()
            .to_string();
        let conf = format!(
            "load_module {module_so};\n\
             worker_processes 1;\n\
             error_log logs/error.log;\n\
             pid logs/nginx.pid;\n\
             events {{ worker_connections 64; }}\n\
             http {{\n\
             include {mime_types};\n\
             access_log logs/access.log;\n\
             {directives}\
             server {{\n\
             listen {};\n\
             server_name 127.0.0.1 localhost;\n\
             root {};\n\
             location / {{ try_files $uri $uri/ =404; }}\n\
             }}\n\
             }}\n",
            self.port,
            doc_root.display(),
        );
        std::fs::write(self.prefix.join("conf/nginx.conf"), conf)
    }

    /// Starts the master (daemonized) and waits for the port.
    fn start(&mut self) -> std::io::Result<()> {
        // The C++ stack needs explicit conf/log paths (its
        // compiled-in defaults point at the system install).
        let mut test = Command::new(&self.binary);
        test.arg("-p").arg(&self.prefix);
        if self.binary == "/usr/sbin/nginx" {
            test.arg("-e")
                .arg("logs/error.log")
                .arg("-c")
                .arg(self.prefix.join("conf/nginx.conf"));
        }
        test.arg("-t");
        let status = test.output()?;
        if !status.status.success() {
            return Err(std::io::Error::other(format!(
                "nginx -t failed: {}",
                String::from_utf8_lossy(&status.stderr)
            )));
        }
        let mut start = Command::new(&self.binary);
        start.arg("-p").arg(&self.prefix);
        if self.binary == "/usr/sbin/nginx" {
            start
                .arg("-e")
                .arg("logs/error.log")
                .arg("-c")
                .arg(self.prefix.join("conf/nginx.conf"));
        }
        let status = start.output()?;
        if !status.status.success() {
            return Err(std::io::Error::other(format!(
                "nginx start failed: {}",
                String::from_utf8_lossy(&status.stderr)
            )));
        }
        for _ in 0..50 {
            if TcpStream::connect_timeout(
                &format!("127.0.0.1:{}", self.port).parse().expect("addr"),
                Duration::from_millis(100),
            )
            .is_ok()
            {
                let pid: u32 = std::fs::read_to_string(self.prefix.join("logs/nginx.pid"))?
                    .trim()
                    .parse()
                    .map_err(std::io::Error::other)?;
                self.master = Some(pid);
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(std::io::Error::other("nginx never listened"))
    }

    /// Stops the master via its pid file.
    fn stop(&mut self) {
        use std::process::Stdio;
        if let Some(pid) = self.master.take() {
            let _ = Command::new("kill")
                .arg("-QUIT")
                .arg(pid.to_string())
                .stderr(Stdio::null())
                .output();
            for _ in 0..50 {
                // `kill -0` succeeding means still alive.
                let alive = Command::new("kill")
                    .arg("-0")
                    .arg(pid.to_string())
                    .stderr(Stdio::null())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
                if !alive {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

impl Drop for NginxInstance {
    fn drop(&mut self) {
        self.stop();
        if !self.failed {
            let _ = std::fs::remove_dir_all(&self.prefix);
        }
    }
}

/// Minimal HTTP response.
struct HttpResponse {
    /// Status code.
    status: u16,
    /// Headers in order (lowercased names, repeats kept).
    headers: Vec<(String, String)>,
    /// Body bytes (de-chunked).
    body: Vec<u8>,
}

impl HttpResponse {
    /// First value for a header (case-insensitive).
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Reads one HTTP/1.1 response (`Content-Length` or chunked).
fn read_response(stream: &mut BufReader<TcpStream>) -> std::io::Result<HttpResponse> {
    let mut head = Vec::new();
    let mut line = String::new();
    loop {
        line.clear();
        if stream.read_line(&mut line)? == 0 {
            break;
        }
        head.extend_from_slice(line.as_bytes());
        if line == "\r\n" {
            break;
        }
    }
    let head_text = String::from_utf8_lossy(&head);
    let mut lines = head_text.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let mut headers = Vec::new();
    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim().to_string();
            if name == "content-length" {
                content_length = value.parse().ok();
            }
            if name == "transfer-encoding" && value.eq_ignore_ascii_case("chunked") {
                chunked = true;
            }
            headers.push((name, value));
        }
    }
    let mut body = Vec::new();
    if chunked {
        loop {
            let mut size_line = String::new();
            if stream.read_line(&mut size_line)? == 0 {
                break;
            }
            let size = usize::from_str_radix(size_line.trim().split(';').next().unwrap_or(""), 16)
                .unwrap_or(0);
            if size == 0 {
                // Trailing CRLF + (ignored) trailers.
                let mut trail = String::new();
                stream.read_line(&mut trail)?;
                break;
            }
            let mut chunk = vec![0u8; size];
            stream.read_exact(&mut chunk)?;
            body.extend_from_slice(&chunk);
            let mut crlf = [0u8; 2];
            stream.read_exact(&mut crlf)?;
        }
    } else if let Some(length) = content_length {
        let mut remaining = vec![0u8; length];
        stream.read_exact(&mut remaining)?;
        body = remaining;
    } else {
        // Connection-close framing: read to EOF.
        stream.read_to_end(&mut body)?;
    }
    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

/// Sends one request, returning the parsed response.
fn http_request(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> std::io::Result<HttpResponse> {
    let mut stream = TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().expect("addr"),
        Duration::from_secs(10),
    )?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    if !body.is_empty() {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("Connection: close\r\n\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    read_response(&mut BufReader::new(stream))
}

/// Splits a case URL into a path + query for the origin file.
fn case_path(url: &str) -> &str {
    let without_scheme = url.split("://").nth(1).unwrap_or(url);
    match without_scheme.find('/') {
        Some(index) => &without_scheme[index..],
        None => "/",
    }
}

/// A live stack under test (Rust or C++ module).
struct LiveStack {
    /// Case currently served (for failure messages).
    case_name: String,
    /// Running instance.
    instance: NginxInstance,
    /// Module `.so` path.
    module_so: String,
}

impl LiveStack {
    /// Boots nginx serving one page case.
    fn boot(
        tag: &str,
        binary: String,
        module_so: String,
        port: u16,
        case: &PageCase,
    ) -> std::io::Result<Self> {
        // Rebuild the Rust module first (cargo no-ops when fresh).
        if tag == "rust" {
            let root = workspace_root();
            let status = Command::new("cargo")
                .arg("build")
                .arg("--release")
                .arg("-p")
                .arg("ps-nginx")
                .current_dir(&root)
                .status()?;
            if !status.success() {
                return Err(std::io::Error::other("cargo build -p ps-nginx failed"));
            }
        }
        let mut instance = NginxInstance::create(binary, tag, port)?;
        let doc_root = instance.prefix.join("html");
        let full_path = case_path(&case.url);
        let path = full_path.split('?').next().unwrap_or(full_path);
        let serve_path = doc_root.join(path.trim_start_matches('/'));
        if let Some(parent) = serve_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Empty origin means "serve nothing" (404 cases).
        if !case.html.is_empty() {
            std::fs::write(&serve_path, &case.html)?;
            // Fixed mtime keeps ETag/Last-Modified identical
            // across stacks and runs (resource cases leave it
            // fresh so C++ considers inputs rewritable).
            if case.fixed_mtime {
                let mtime = std::time::UNIX_EPOCH
                    + std::time::Duration::from_secs(corpus::FIXED_MTIME_SECS);
                std::fs::File::options()
                    .write(true)
                    .open(&serve_path)?
                    .set_modified(mtime)?;
            }
        }
        for (path, bytes) in &case.extra {
            let extra_path = doc_root.join(path.trim_start_matches('/'));
            if let Some(parent) = extra_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&extra_path, bytes)?;
            let mtime =
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(corpus::FIXED_MTIME_SECS);
            std::fs::File::options()
                .write(true)
                .open(&extra_path)?
                .set_modified(mtime)?;
        }
        instance.write_case_config(&module_so, case, &doc_root)?;
        instance.start()?;
        Ok(LiveStack {
            case_name: case.name.clone(),
            instance,
            module_so,
        })
    }

    /// Fetches one path from the booted stack.
    fn fetch(&mut self, path: &str) -> std::io::Result<HttpResponse> {
        http_request(self.instance.port, "GET", path, &[], &[])
    }

    /// Runs one request against the booted stack.
    fn run_case(&mut self, case: &PageCase) -> std::io::Result<Vec<AssertionResult>> {
        let response = self.fetch(case_path(&case.url))?;
        let mut results = Vec::new();
        for assertion in &case.assertions {
            let filled = match assertion {
                Assertion::ByteEqual { expected, .. } => Assertion::ByteEqual {
                    expected: expected.clone(),
                    actual: response.body.clone(),
                },
                Assertion::HeaderEqual { name, expected, .. } => Assertion::HeaderEqual {
                    name: name.clone(),
                    expected: expected.clone(),
                    actual: response
                        .header(&name.to_ascii_lowercase())
                        .unwrap_or_default()
                        .to_string(),
                },
                Assertion::StatusEqual { expected, .. } => Assertion::StatusEqual {
                    expected: *expected,
                    actual: response.status,
                },
                other => other.clone(),
            };
            results.push(evaluate(&filled));
        }
        if results.iter().any(|result| !result.passed) {
            self.mark_failed(&response);
        }
        Ok(results)
    }

    /// Keeps the prefix and dumps the failure for forensics.
    fn mark_failed(&mut self, response: &HttpResponse) {
        self.instance.failed = true;
        eprintln!("live case failed: {}", self.case_name);
        eprintln!("prefix kept at: {}", self.instance.prefix.display());
        eprintln!("status: {}", response.status);
        eprintln!(
            "body (first 500 bytes): {}",
            String::from_utf8_lossy(&response.body[..response.body.len().min(500)])
        );
    }
}

/// Drives the Rust stack.
struct RustRunner {
    /// nginx binary.
    binary: String,
    /// Module `.so`.
    module_so: String,
    /// Probe port.
    port: u16,
}

/// Drives the C++ stack.
struct CppRunner {
    /// nginx binary.
    binary: String,
    /// Module `.so`.
    module_so: String,
    /// Probe port.
    port: u16,
}

/// Boots one stack, runs one page case, tears down.
fn run_live_page(
    tag: &str,
    binary: &str,
    module_so: &str,
    port: u16,
    case: &PageCase,
) -> Vec<AssertionResult> {
    match LiveStack::boot(tag, binary.to_string(), module_so.to_string(), port, case) {
        Ok(mut stack) => match stack.run_case(case) {
            Ok(results) => results,
            Err(error) => vec![AssertionResult {
                kind: "harness",
                passed: false,
                detail: format!("request failed: {error}"),
            }],
        },
        Err(error) => vec![AssertionResult {
            kind: "harness",
            passed: false,
            detail: format!("boot failed: {error}"),
        }],
    }
}

impl Runner for RustRunner {
    fn run_page(&self, case: &PageCase) -> Vec<AssertionResult> {
        run_live_page("rust", &self.binary, &self.module_so, self.port, case)
    }

    fn run_config(&self, _case: &ConfigCase) -> Vec<AssertionResult> {
        vec![]
    }
}

impl Runner for CppRunner {
    fn run_page(&self, case: &PageCase) -> Vec<AssertionResult> {
        run_live_page("cpp", &self.binary, &self.module_so, self.port, case)
    }

    fn run_config(&self, _case: &ConfigCase) -> Vec<AssertionResult> {
        vec![]
    }
}

/// Hands out unique probe ports (parallel-safe).
fn next_port() -> u16 {
    static NEXT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
    let base: u16 = knob("PS_COMPAT_PORT_BASE", "18090".to_string())
        .parse()
        .expect("port");
    base + NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

/// Builds the Rust runner from knobs.
fn rust_runner() -> RustRunner {
    let root = workspace_root();
    let staged = root
        .parent()
        .expect("projects dir")
        .join(".nginx-build/nginx-1.31.8/objs/nginx");
    RustRunner {
        binary: knob("PS_COMPAT_RUST_NGINX", staged.display().to_string()),
        module_so: knob(
            "PS_COMPAT_RUST_SO",
            root.join("target/release/libps_nginx.so")
                .display()
                .to_string(),
        ),
        port: next_port(),
    }
}

/// Builds the C++ runner from knobs.
fn cpp_runner() -> CppRunner {
    CppRunner {
        binary: knob("PS_COMPAT_CPP_NGINX", "/usr/sbin/nginx".to_string()),
        module_so: knob(
            "PS_COMPAT_CPP_SO",
            "/usr/lib/nginx/modules/ngx_pagespeed.so".to_string(),
        ),
        port: next_port(),
    }
}

/// Starter page case: options on, tiny HTML, version header.
fn smoke_case() -> PageCase {
    PageCase {
        name: "smoke-html".to_string(),
        url: "http://127.0.0.1/page.html".to_string(),
        html: b"<html><head><title>t</title></head><body><p>hi</p></body></html>".to_vec(),
        extra: vec![],
        fixed_mtime: true,
        directives: vec![
            ("on".to_string(), String::new()),
            (
                "FileCachePath".to_string(),
                corpus::CACHE_PLACEHOLDER.to_string(),
            ),
        ],
        assertions: vec![],
    }
}

#[test]
fn rust_stack_serves_html() {
    if !live_enabled() {
        eprintln!("skipped: set PS_COMPAT_LIVE=1");
        return;
    }
    let runner = rust_runner();
    let mut case = smoke_case();
    case.assertions = vec![Assertion::HeaderEqual {
        name: "X-Page-Speed".to_string(),
        expected: env!("CARGO_PKG_VERSION").to_string(),
        actual: String::new(),
    }];
    let results = runner.run_page(&case);
    for result in &results {
        assert!(result.passed, "{}", result.detail);
    }
    assert!(!results.is_empty());
}

#[test]
fn cpp_stack_serves_html() {
    if !live_enabled() {
        eprintln!("skipped: set PS_COMPAT_LIVE=1");
        return;
    }
    // Boots the C++ stack and serves one page; any harness
    // failure surfaces as a failed result. C++ stamps its own
    // version, so byte/header comparisons land with the record
    // mode in 4b.
    let results = cpp_runner().run_page(&smoke_case());
    for result in &results {
        assert!(result.passed, "{}", result.detail);
    }
}

/// Looks up a case's mtime pinning (recorded cases predate the
/// flag, so the live defs are authoritative).
fn def_fixed_mtime(name: &str) -> bool {
    corpus::CASES
        .iter()
        .find(|def| def.name == name)
        .is_none_or(|def| def.fixed_mtime)
}

/// Builds a [`PageCase`] from corpus inputs.
fn corpus_page(
    name: &str,
    path: &str,
    html: &[u8],
    extra: &[(String, Vec<u8>)],
    fixed_mtime: bool,
    directives: &[(String, String)],
) -> PageCase {
    PageCase {
        name: name.to_string(),
        url: format!("http://127.0.0.1{path}"),
        html: html.to_vec(),
        extra: extra.to_vec(),
        fixed_mtime,
        directives: directives.to_vec(),
        assertions: vec![],
    }
}

/// Records (`PS_COMPAT_RECORD=1`) or compares the page corpus.
#[test]
fn page_corpus() {
    use corpus::{normalize, RecordedCase};
    if !live_enabled() {
        eprintln!("skipped: set PS_COMPAT_LIVE=1");
        return;
    }
    if std::env::var("PS_COMPAT_RECORD").as_deref() == Ok("1") {
        let runner = cpp_runner();
        let mut recorded = Vec::new();
        for def in corpus::CASES {
            let directives: Vec<(String, String)> = def
                .directives
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect();
            let extra: Vec<(String, Vec<u8>)> = def
                .extra
                .iter()
                .map(|(path, bytes)| (path.to_string(), bytes.to_vec()))
                .collect();
            let case = corpus_page(
                def.name,
                def.path,
                def.html.unwrap_or_default(),
                &extra,
                def.fixed_mtime,
                &directives,
            );
            let mut stack = LiveStack::boot(
                "cpp",
                runner.binary.clone(),
                runner.module_so.clone(),
                next_port(),
                &case,
            )
            .unwrap_or_else(|error| panic!("record boot {}: {error}", def.name));
            let mut response = stack
                .fetch(def.path)
                .unwrap_or_else(|error| panic!("record fetch {}: {error}", def.name));
            for _ in 1..def.fetches {
                response = stack
                    .fetch(def.path)
                    .unwrap_or_else(|error| panic!("record fetch {}: {error}", def.name));
            }
            if def.poll_optimized {
                // C++ optimizes asynchronously; poll until the
                // body differs from the origin input (or cap).
                let origin = def.html.unwrap_or_default();
                for _ in 0..20 {
                    if response.body.as_slice() != origin {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    response = stack
                        .fetch(def.path)
                        .unwrap_or_else(|error| panic!("record fetch {}: {error}", def.name));
                }
                if response.body.as_slice() == origin {
                    eprintln!("note {}: body never optimized within cap", def.name);
                }
            }
            let (headers, body) = normalize(&response.headers, &response.body);
            recorded.push(RecordedCase {
                name: def.name.to_string(),
                path: def.path.to_string(),
                html: def.html.unwrap_or_default().to_vec(),
                extra,
                directives,
                status: response.status,
                headers,
                body,
                known_gap: def.known_gap.map(str::to_string),
            });
        }
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        corpus::write_recorded(&recorded, &manifest).expect("write corpus");
        eprintln!(
            "recorded {} cases to tests/corpus_data/pages.rs (review + commit)",
            recorded.len()
        );
        return;
    }
    let recorded = corpus::recorded_cases();
    if recorded.is_empty() {
        eprintln!("no recorded cases: run with PS_COMPAT_RECORD=1 first");
        return;
    }
    let runner = rust_runner();
    let mut failures = 0;
    for case in &recorded {
        // Compare replays the RECORDED INPUT html, not the
        // expected body.
        let page = corpus_page(
            &case.name,
            &case.path,
            &case.html,
            &case.extra,
            def_fixed_mtime(&case.name),
            &case.directives,
        );
        let mut stack = match LiveStack::boot(
            "rust",
            runner.binary.clone(),
            runner.module_so.clone(),
            next_port(),
            &page,
        ) {
            Ok(stack) => stack,
            Err(error) => {
                eprintln!("FAIL {}: boot: {error}", case.name);
                failures += 1;
                continue;
            }
        };
        let def = corpus::CASES.iter().find(|def| def.name == case.name);
        let fetches = def.map(|def| def.fetches).unwrap_or(1);
        let poll = def.is_some_and(|def| def.poll_optimized);
        let mut response = match stack.fetch(&case.path) {
            Ok(response) => response,
            Err(error) => {
                eprintln!("FAIL {}: fetch: {error}", case.name);
                failures += 1;
                continue;
            }
        };
        for _ in 1..fetches {
            response = match stack.fetch(&case.path) {
                Ok(response) => response,
                Err(error) => {
                    eprintln!("FAIL {}: fetch: {error}", case.name);
                    failures += 1;
                    continue;
                }
            };
        }
        if poll {
            for _ in 0..20 {
                if response.body.as_slice() != case.html.as_slice() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_secs(3));
                response = match stack.fetch(&case.path) {
                    Ok(response) => response,
                    Err(error) => {
                        eprintln!("FAIL {}: fetch: {error}", case.name);
                        failures += 1;
                        continue;
                    }
                };
            }
        }
        let (headers, body) = normalize(&response.headers, &response.body);
        // Poll cases compare volatile-normalized headers (TTL
        // countdowns differ by stack timing).
        let (headers, expected) = if poll {
            (
                corpus::normalize_volatile(&headers),
                corpus::normalize_volatile(&case.headers),
            )
        } else {
            (headers, case.headers.clone())
        };
        let mut case_failures = Vec::new();
        for result in [evaluate(&Assertion::StatusEqual {
            expected: case.status,
            actual: response.status,
        })] {
            if !result.passed {
                case_failures.push(result.detail.clone());
            }
        }
        // Image responses compare pixels (encoders differ) and
        // skip the content-hash etag (likewise encoder-bound).
        let is_image = expected
            .iter()
            .any(|(name, value)| name == "content-type" && value.starts_with("image/"));
        let (headers, expected) = if is_image {
            let strip = |headers: &[(String, String)]| {
                headers
                    .iter()
                    .filter(|(name, _)| name != "etag")
                    .cloned()
                    .collect::<Vec<_>>()
            };
            (strip(&headers), strip(&expected))
        } else {
            (headers, expected)
        };
        if headers != expected {
            case_failures.push(format!(
                "headers differ:\n  expected: {expected:?}\n  actual:   {headers:?}"
            ));
        }
        let body_result = if is_image {
            evaluate(&Assertion::ImagePixelsEqual {
                expected: case.body.clone(),
                actual: body.clone(),
            })
        } else {
            evaluate(&Assertion::ByteEqual {
                expected: case.body.clone(),
                actual: body.clone(),
            })
        };
        if !body_result.passed {
            if is_image {
                case_failures.push(format!("image differs: {}", body_result.detail));
            } else {
                let preview = |bytes: &[u8]| {
                    String::from_utf8_lossy(&bytes[..bytes.len().min(300)]).into_owned()
                };
                case_failures.push(format!(
                    "body differs ({} vs {} bytes):\n  expected: {:?}\n  actual:   {:?}",
                    case.body.len(),
                    body.len(),
                    preview(&case.body),
                    preview(&body)
                ));
            }
        }
        match (&case.known_gap, case_failures.is_empty()) {
            (Some(reason), false) => {
                eprintln!("KNOWN GAP {} ({reason}): still diverging", case.name);
            }
            (Some(reason), true) => {
                eprintln!("GAP CLOSED {} ({reason}): now matches!", case.name);
            }
            (None, true) => {
                eprintln!("ok {}", case.name);
            }
            (None, false) => {
                eprintln!("FAIL {}", case.name);
                for failure in &case_failures {
                    eprintln!("  {failure}");
                }
                failures += 1;
            }
        }
    }
    assert_eq!(0, failures, "corpus divergences above");
}

/// Records (`PS_COMPAT_RECORD=1`) or compares the config corpus.
#[test]
fn config_corpus() {
    use corpus::{normalize, RecordedConfig};
    if !live_enabled() {
        eprintln!("skipped: set PS_COMPAT_LIVE=1");
        return;
    }
    if std::env::var("PS_COMPAT_RECORD").as_deref() == Ok("1") {
        let runner = cpp_runner();
        let mut recorded = Vec::new();
        for def in corpus::CONFIGS {
            let directives: Vec<(String, String)> = def
                .directives
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect();
            let case = corpus_page(
                def.name,
                def.path,
                corpus::CONFIG_PROBE_HTML,
                &[],
                true,
                &directives,
            );
            let mut stack = match LiveStack::boot(
                "cpp",
                runner.binary.clone(),
                runner.module_so.clone(),
                next_port(),
                &case,
            ) {
                Ok(stack) => stack,
                Err(error) => {
                    eprintln!("record {}: boot failed ({error})", def.name);
                    recorded.push(RecordedConfig {
                        name: def.name.to_string(),
                        path: def.path.to_string(),
                        directives,
                        boot_ok: false,
                        status: 0,
                        headers: Vec::new(),
                        body: Vec::new(),
                        known_gap: def.known_gap.map(str::to_string),
                    });
                    continue;
                }
            };
            let response = stack
                .fetch(def.path)
                .unwrap_or_else(|error| panic!("record fetch {}: {error}", def.name));
            let (headers, body) = normalize(&response.headers, &response.body);
            recorded.push(RecordedConfig {
                name: def.name.to_string(),
                path: def.path.to_string(),
                directives,
                boot_ok: true,
                status: response.status,
                headers,
                body,
                known_gap: def.known_gap.map(str::to_string),
            });
        }
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        corpus::write_recorded_configs(&recorded, &manifest).expect("write corpus");
        eprintln!("recorded {} config cases (review + commit)", recorded.len());
        return;
    }
    let recorded = corpus::recorded_configs();
    if recorded.is_empty() {
        eprintln!("no recorded config cases: run with PS_COMPAT_RECORD=1 first");
        return;
    }
    let runner = rust_runner();
    let mut failures = 0;
    for case in &recorded {
        let page = corpus_page(
            &case.name,
            &case.path,
            corpus::CONFIG_PROBE_HTML,
            &[],
            true,
            &case.directives,
        );
        let mut stack = match LiveStack::boot(
            "rust",
            runner.binary.clone(),
            runner.module_so.clone(),
            next_port(),
            &page,
        ) {
            Ok(stack) => stack,
            Err(_) => {
                if case.boot_ok {
                    eprintln!("FAIL {}: rust boot failed, C++ booted", case.name);
                    failures += 1;
                } else {
                    eprintln!("ok {} (both reject)", case.name);
                }
                continue;
            }
        };
        if !case.boot_ok {
            eprintln!("FAIL {}: rust booted, C++ rejected", case.name);
            failures += 1;
            continue;
        }
        let response = match stack.fetch(&case.path) {
            Ok(response) => response,
            Err(error) => {
                eprintln!("FAIL {}: fetch: {error}", case.name);
                failures += 1;
                continue;
            }
        };
        let (headers, body) = normalize(&response.headers, &response.body);
        let mut case_failures = Vec::new();
        if response.status != case.status {
            case_failures.push(format!(
                "status: expected {}, got {}",
                case.status, response.status
            ));
        }
        if headers != case.headers {
            case_failures.push(format!(
                "headers differ:\n  expected: {:?}\n  actual:   {headers:?}",
                case.headers
            ));
        }
        if body != case.body {
            let preview =
                |bytes: &[u8]| String::from_utf8_lossy(&bytes[..bytes.len().min(300)]).into_owned();
            case_failures.push(format!(
                "body differs ({} vs {} bytes):\n  expected: {:?}\n  actual:   {:?}",
                case.body.len(),
                body.len(),
                preview(&case.body),
                preview(&body)
            ));
        }
        match (&case.known_gap, case_failures.is_empty()) {
            (Some(reason), false) => {
                eprintln!("KNOWN GAP {} ({reason}): still diverging", case.name);
            }
            (Some(reason), true) => {
                eprintln!("GAP CLOSED {} ({reason}): now matches!", case.name);
            }
            (None, true) => {
                eprintln!("ok {}", case.name);
            }
            (None, false) => {
                eprintln!("FAIL {}", case.name);
                for failure in &case_failures {
                    eprintln!("  {failure}");
                }
                failures += 1;
            }
        }
    }
    assert_eq!(0, failures, "config divergences above");
}
