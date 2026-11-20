//! `pagespeed` directive dispatch (`ps_configure`, `ps_init_dir`).
//!
//! Ports the directive plumbing from `ngx_pagespeed.cc`: the
//! `on`/`off` master switch, the `gzip on|off` toggle, the
//! `LogDir`/`FileCachePath` directory prep (config-time-only
//! filesystem syscalls), and the `$ps_dollar` script variable.
//! Option parsing itself lives in ps-config
//! (`apply_nginx_directive`); the nginx callback wiring that
//! calls into this module is `module.rs`.

use ngx::ffi::{ngx_http_request_t, ngx_http_variable_value_t, ngx_int_t, ngx_pool_t, NGX_OK};
use std::path::Path;

/// Argument array size (`NGX_PAGESPEED_MAX_ARGS`); the
/// `n_args <= MAX` check never fires since the commands table
/// admits at most 5 words after `pagespeed`.
pub const MAX_DIRECTIVE_ARGS: usize = 10;

/// Directive classification (`ps_configure` dispatch).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Directive {
    /// Bare `on` (also arms the gzip setter; Slice 6).
    On,
    /// Bare `off` (also disarms the gzip setter; Slice 6).
    Off,
    /// `gzip on|off` toggle (setter call; Slice 6).
    Gzip(bool),
    /// `gzip` with a bad value.
    InvalidGzip,
    /// `LogDir`/`FileCachePath` need directory prep first.
    DirPrep,
    /// Ordinary option line for `apply_nginx_directive`.
    Options,
}

/// Classifies directive arguments (everything after `pagespeed`).
#[must_use]
pub fn classify(args: &[&str]) -> Directive {
    if args.len() == 1 {
        if args[0].eq_ignore_ascii_case("on") {
            return Directive::On;
        }
        if args[0].eq_ignore_ascii_case("off") {
            return Directive::Off;
        }
    }
    // Case-sensitive `compare`, unlike the on/off switch.
    if args.len() == 2 && args[0] == "gzip" {
        if args[1] == "on" {
            return Directive::Gzip(true);
        }
        if args[1] == "off" {
            return Directive::Gzip(false);
        }
        return Directive::InvalidGzip;
    }
    if args.len() == 2
        && (args[0].eq_ignore_ascii_case("LogDir") || args[0].eq_ignore_ascii_case("FileCachePath"))
    {
        return Directive::DirPrep;
    }
    Directive::Options
}

/// Directory prep outcome (`ps_init_dir` past validation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirPrep {
    /// Directory already exists and is a directory.
    Exists,
    /// Directory was created by this call.
    Created,
}

/// Prepares a directive directory (`ps_init_dir`, validation +
/// creation half).
///
/// Returns the pool-string error text on failure, exactly as the
/// C++ reports it. Ownership fixup (`stat`/`chown` when root) is
/// `fix_dir_ownership`, called by the FFI wrapper with live
/// nginx state.
pub fn prepare_dir(directive: &str, path: &str) -> Result<DirPrep, String> {
    if path.is_empty() || !path.starts_with('/') {
        return Err(format!("{directive} {path} must start with a slash"));
    }
    let fs_path = Path::new(path);
    match std::fs::metadata(fs_path) {
        Ok(meta) if meta.is_dir() => Ok(DirPrep::Exists),
        _ => std::fs::create_dir_all(fs_path)
            .map(|()| DirPrep::Created)
            .map_err(|_| {
                format!("{directive} path {path} does not exist and could not be created.")
            }),
    }
}

/// Ownership-fixup decision inputs (`ps_init_dir` chown half).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ownership {
    /// Effective uid of this process (`geteuid`).
    pub euid: u32,
    /// Owner uid of the directory (`stat`).
    pub dir_uid: u32,
    /// nginx worker user id (`ccf->user`).
    pub worker_uid: u32,
    /// nginx worker group id (`ccf->group`).
    pub worker_gid: u32,
}

/// Ownership-fixup outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnershipAction {
    /// Not root: ownership stays as-is.
    Keep,
    /// Already owned by the worker user: nothing to do.
    AlreadyOwned,
    /// chown to (worker_uid, worker_gid).
    Chown(u32, u32),
}

/// Decides the ownership fixup (`ps_init_dir` chown half).
#[must_use]
pub const fn decide_ownership(facts: Ownership) -> OwnershipAction {
    if facts.euid != 0 {
        return OwnershipAction::Keep;
    }
    if facts.dir_uid != facts.worker_uid {
        return OwnershipAction::Chown(facts.worker_uid, facts.worker_gid);
    }
    OwnershipAction::AlreadyOwned
}

/// Runs the ownership fixup syscalls (`stat` + `chown`).
///
/// Returns the pool-string error text on failure, exactly as the
/// C++ reports it. Config-time only: never call on the request
/// path.
pub fn fix_dir_ownership(
    directive: &str,
    path: &str,
    worker_uid: u32,
    worker_gid: u32,
) -> Result<(), String> {
    // SAFETY: `geteuid` takes no arguments and always succeeds.
    let euid = unsafe { libc::geteuid() };
    if euid != 0 {
        return Ok(());
    }
    let c_path =
        std::ffi::CString::new(path).map_err(|_| format!("{directive} {path} stat() failed"))?;
    // SAFETY: `stat` on a valid NUL-terminated path with a live
    // output struct.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    let rc = unsafe { libc::stat(c_path.as_ptr(), &mut st) };
    if rc != 0 {
        return Err(format!("{directive} {path} stat() failed"));
    }
    match decide_ownership(Ownership {
        euid,
        dir_uid: st.st_uid,
        worker_uid,
        worker_gid,
    }) {
        OwnershipAction::Chown(uid, gid) => {
            // SAFETY: `chown` on the same valid path.
            let rc = unsafe { libc::chown(c_path.as_ptr(), uid, gid) };
            if rc != 0 {
                return Err(format!("{directive} {path} unable to set permissions"));
            }
            Ok(())
        }
        OwnershipAction::Keep | OwnershipAction::AlreadyOwned => Ok(()),
    }
}

/// Copies bytes into a pool NUL-terminated string
/// (`string_piece_to_pool_string`).
///
/// # Safety
///
/// `pool` must point at a live `ngx_pool_t`. Returns null on
/// allocation failure, like the C++.
pub unsafe fn pool_cstring(pool: *mut ngx_pool_t, text: &str) -> *mut std::ffi::c_char {
    // SAFETY: caller guarantees a live pool; `ngx_palloc` takes
    // the pool plus a byte count.
    let len = text.len() + 1;
    let out = unsafe { ngx::ffi::ngx_palloc(pool, len) as *mut u8 };
    if out.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `out` points at `len` pool bytes; the copy fills
    // the first `len - 1` and the NUL lands last.
    unsafe {
        std::ptr::copy_nonoverlapping(text.as_ptr(), out, text.len());
        *out.add(text.len()) = 0;
    }
    out as *mut std::ffi::c_char
}

/// `$ps_dollar` script variable name (`ps_configure`).
pub const DOLLAR_VARIABLE: &str = "ps_dollar";

/// Whether to register `$ps_dollar`: script-variables mode is
/// anything but off (`ps_configure` gate).
#[must_use]
pub fn should_register_dollar(mode: ps_config::options::ScriptVariables) -> bool {
    !matches!(mode, ps_config::options::ScriptVariables::Off)
}

/// `$ps_dollar` expansion: a literal `$` (`ps_dollar`).
pub const DOLLAR_VALUE: &[u8] = b"$";

/// Script-variable getter expanding to `$` (`ps_dollar`).
///
/// # Safety
///
/// `v` must point at a live `ngx_http_variable_value_t`. The
/// request is unused; `data` is unused.
pub unsafe extern "C" fn ps_dollar(
    _r: *mut ngx_http_request_t,
    v: *mut ngx_http_variable_value_t,
    _data: usize,
) -> ngx_int_t {
    // SAFETY: caller guarantees a live value struct.
    unsafe {
        (*v).set_valid(1);
        (*v).set_no_cacheable(0);
        (*v).set_not_found(0);
        // Const-cast of static bytes, as the C++ `const_cast`.
        (*v).data = DOLLAR_VALUE.as_ptr() as *mut u8;
        (*v).set_len(DOLLAR_VALUE.len() as u32);
    }
    NGX_OK as ngx_int_t
}

/// Reads a variable value's bytes (test helper over the FFI type).
///
/// # Safety
///
/// `v` must point at a live value struct with `len` readable
/// bytes at `data`.
#[cfg(test)]
unsafe fn value_bytes(v: *const ngx_http_variable_value_t) -> Vec<u8> {
    // SAFETY: caller guarantees as documented.
    unsafe { std::slice::from_raw_parts((*v).data, (*v).len() as usize).to_vec() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    #[test]
    fn classification_matches_cxx() {
        assert_eq!(Directive::On, classify(&["on"]));
        assert_eq!(Directive::On, classify(&["ON"]));
        assert_eq!(Directive::Off, classify(&["off"]));
        assert_eq!(Directive::Gzip(true), classify(&["gzip", "on"]));
        assert_eq!(Directive::Gzip(false), classify(&["gzip", "off"]));
        assert_eq!(Directive::InvalidGzip, classify(&["gzip", "maybe"]));
        // `gzip` match is case-sensitive, unlike on/off.
        assert_eq!(Directive::Options, classify(&["Gzip", "on"]));
        assert_eq!(Directive::DirPrep, classify(&["FileCachePath", "/x"]));
        assert_eq!(Directive::DirPrep, classify(&["logdir", "/x"]));
        assert_eq!(
            Directive::Options,
            classify(&["RewriteLevel", "CoreFilters"])
        );
        assert_eq!(Directive::Options, classify(&["RewriteLevel"]));
        assert_eq!(Directive::Options, classify(&[]));
        assert_eq!(Directive::Options, classify(&["a", "b", "c"]));
        assert_eq!(MAX_DIRECTIVE_ARGS, 10);
    }

    #[test]
    fn prepare_dir_validates_and_creates() {
        assert_eq!(
            Err("FileCachePath relative must start with a slash".to_string()),
            prepare_dir("FileCachePath", "relative")
        );
        assert_eq!(
            Err("LogDir  must start with a slash".to_string()),
            prepare_dir("LogDir", "")
        );
        let base = std::env::temp_dir().join(format!("ps-nginx-dirprep-{}", std::process::id()));
        let nested = base.join("a").join("b");
        let path = nested.to_str().unwrap().to_string();
        assert_eq!(Ok(DirPrep::Created), prepare_dir("FileCachePath", &path));
        assert!(nested.is_dir());
        assert_eq!(Ok(DirPrep::Exists), prepare_dir("FileCachePath", &path));
        // A regular file is not a directory: creation fails.
        let file = base.join("file");
        std::fs::write(&file, b"x").unwrap();
        let file_path = file.to_str().unwrap().to_string();
        assert_eq!(
            Err(format!(
                "LogDir path {file_path} does not exist and could not be created."
            )),
            prepare_dir("LogDir", &file_path)
        );
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn ownership_decisions() {
        // Non-root keeps ownership without even stating.
        assert_eq!(
            OwnershipAction::Keep,
            decide_ownership(Ownership {
                euid: 1000,
                dir_uid: 0,
                worker_uid: 0,
                worker_gid: 0
            })
        );
        // Root chowns only when the owner differs.
        assert_eq!(
            OwnershipAction::Chown(33, 33),
            decide_ownership(Ownership {
                euid: 0,
                dir_uid: 0,
                worker_uid: 33,
                worker_gid: 33
            })
        );
        assert_eq!(
            OwnershipAction::AlreadyOwned,
            decide_ownership(Ownership {
                euid: 0,
                dir_uid: 33,
                worker_uid: 33,
                worker_gid: 33
            })
        );
    }

    #[test]
    fn fix_ownership_as_non_root_is_noop() {
        // SAFETY: pure geteuid check; no nginx state involved.
        // Test runners are non-root, so this returns before stat.
        assert!(fix_dir_ownership("FileCachePath", "/nonexistent-ps-test", 33, 33).is_ok());
    }

    #[test]
    fn dollar_handler_expands() {
        // SAFETY: fabricated value struct; request unused.
        unsafe {
            let mut v: ngx_http_variable_value_t = MaybeUninit::zeroed().assume_init();
            let rc = ps_dollar(std::ptr::null_mut(), &mut v, 0);
            assert_eq!(NGX_OK as ngx_int_t, rc);
            assert_eq!(1, v.valid());
            assert_eq!(0, v.no_cacheable());
            assert_eq!(0, v.not_found());
            assert_eq!(b"$".to_vec(), value_bytes(&v));
            assert_eq!("ps_dollar", DOLLAR_VARIABLE);
        }
    }

    #[test]
    fn max_args_covers_take5() {
        // Commands take at most 5 words; the args array must hold
        // all of them plus the directive word.
        assert!(MAX_DIRECTIVE_ARGS >= 5);
    }

    #[test]
    fn dollar_registers_when_scripts_on() {
        use ps_config::options::ScriptVariables;
        assert!(!should_register_dollar(ScriptVariables::Off));
        assert!(should_register_dollar(ScriptVariables::LegacyRestricted));
        assert!(should_register_dollar(ScriptVariables::All));
    }
}
