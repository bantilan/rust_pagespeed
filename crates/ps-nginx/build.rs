//! Module-signature probe (`NGX_MODULE_SIGNATURE`).
//!
//! nginx refuses a dynamic module whose signature string differs
//! from its own, so the signature must track the nginx tree the
//! bindings came from — not a baked constant. This script
//! compiles and runs a tiny C probe against that tree (same
//! `NGINX_SOURCE_DIR` / `NGINX_BUILD_DIR` resolution as
//! nginx-sys) and exposes the result as
//! `PS_NGINX_MODULE_SIGNATURE` for [`crate::module`].
//!
//! Requires a C compiler and a configured tree (one with
//! `objs/ngx_auto_config.h`); both already hold wherever this
//! crate builds, since bindgen needs them too.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=NGINX_SOURCE_DIR");
    println!("cargo:rerun-if-env-changed=NGINX_BUILD_DIR");
    println!("cargo::rustc-check-cfg=cfg(ngx_http_v2)");
    println!("cargo::rustc-check-cfg=cfg(ngx_http_gzip)");
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let (source_dir, build_dir) = resolve_dirs();
    // Feature gates mirroring the C++ `#if`s: the `http2`
    // variable lookup and the gzip scan only exist where the
    // target tree has those modules.
    let auto_config =
        std::fs::read_to_string(build_dir.join("ngx_auto_config.h")).expect("read auto_config");
    for (macro_name, cfg) in [
        ("NGX_HTTP_V2", "ngx_http_v2"),
        ("NGX_HTTP_GZIP", "ngx_http_gzip"),
    ] {
        // Presence of the module's define is the test.
        let enabled = auto_config.lines().any(|line| {
            let line = line.trim();
            line.starts_with("#define")
                && line
                    .split_whitespace()
                    .nth(1)
                    .is_some_and(|name| name == macro_name)
        });
        if enabled {
            println!("cargo:rustc-cfg={cfg}");
        }
    }
    let probe_c = out_dir.join("sig_probe.c");
    std::fs::write(
        &probe_c,
        "#include <stdio.h>\n\
         #include <ngx_config.h>\n\
         #include <ngx_core.h>\n\
         int main(void) { printf(\"%s\", NGX_MODULE_SIGNATURE); return 0; }\n",
    )
    .expect("write probe");
    let probe_bin = out_dir.join("sig_probe");
    let include = |dir: &str| format!("-I{}", source_dir.join(dir).display());
    let status = Command::new("cc")
        .arg(&probe_c)
        .arg("-o")
        .arg(&probe_bin)
        .arg(format!("-I{}", build_dir.display()))
        .arg(include("src/core"))
        .arg(include("src/os/unix"))
        .arg(include("src/event"))
        .arg(include("src/http"))
        .arg(include("src/http/v2"))
        .status()
        .expect("run cc (a C compiler must be installed)");
    assert!(
        status.success(),
        "signature probe did not compile against {}",
        source_dir.display()
    );
    let output = Command::new(&probe_bin)
        .output()
        .expect("run signature probe");
    assert!(output.status.success(), "signature probe failed");
    let signature = String::from_utf8(output.stdout).expect("signature is text");
    assert!(
        signature.starts_with(|c: char| c.is_ascii_digit()) && signature.contains(','),
        "unexpected signature: {signature:?}"
    );
    println!("cargo:rustc-env=PS_NGINX_MODULE_SIGNATURE={signature}");
}

/// Resolves the nginx tree (mirrors nginx-sys).
fn resolve_dirs() -> (PathBuf, PathBuf) {
    let source = env::var_os("NGINX_SOURCE_DIR").map(PathBuf::from);
    let build = env::var_os("NGINX_BUILD_DIR").map(PathBuf::from);
    match (source, build) {
        (Some(source), Some(build)) => (source, build),
        (Some(source), None) => {
            let build = source.join("objs");
            (source, build)
        }
        (None, Some(build)) => {
            let source = build.parent().expect("build dir has a parent").to_owned();
            (source, build)
        }
        (None, None) => panic!(
            "neither NGINX_SOURCE_DIR nor NGINX_BUILD_DIR is set \
             (the workspace .cargo/config.toml normally provides one)"
        ),
    }
}
