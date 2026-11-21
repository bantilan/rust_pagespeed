//! Configuration create/merge (`ps_create_*`, `ps_merge_*`).
//!
//! Ports the nginx configuration half of `ngx_pagespeed.cc`:
//! the main/server/location conf shapes, `NgxRewriteOptions`
//! (core options + system options + script lines), the
//! order-sensitive option merge, the server context
//! (`NgxServerContext`: identity, global options, http2 index,
//! initialized latch), and the FileCachePath validation. The
//! driver factory config half lives on the main conf; its
//! threads, caches, and live services join in later Slice 6
//! turns.

use crate::factory::DriverFactory;
use crate::message::NgxMessageHandler;
use ngx::ffi::ngx_int_t;
use ps_config::options::{NginxSystemOptions, RewriteOptions};
use ps_config::scope::Scope;

/// One script-variable line (`ScriptLine`): raw directive words
/// plus the scope they were parsed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptLine {
    /// Directive words, e.g. `["RewriteLevel", "CoreFilters"]`.
    pub args: Vec<String>,
    /// Scope the line was parsed at (request-time applies here).
    pub scope: Scope,
}

/// Nginx options object (`NgxRewriteOptions`): core options plus
/// the system subset plus script lines plus the inherited-scripts
/// flag.
#[derive(Debug, Clone, Default)]
pub struct NgxOptions {
    // NOTE: no `PartialEq`: the core options type has none.
    /// Core rewrite options.
    pub core: RewriteOptions,
    /// System options subset (paths).
    pub system: NginxSystemOptions,
    /// Script-variable lines in parse order.
    pub script_lines: Vec<ScriptLine>,
    /// `clear_inherited_scripts_` (per-level; read by merge).
    pub clear_inherited_scripts: bool,
}

impl NgxOptions {
    /// Creates empty options (`new NgxRewriteOptions`).
    #[must_use]
    pub fn new() -> Self {
        NgxOptions::default()
    }

    /// Clones options (`Clone`: script lines plus `Merge`).
    ///
    /// Note the flag does not propagate: base-class `Merge`
    /// cannot see it, so the clone starts cleared, like C++.
    #[must_use]
    pub fn clone_with(&self) -> Self {
        let mut cloned = NgxOptions {
            core: self.core.clone(),
            system: self.system.clone(),
            script_lines: Vec::new(),
            clear_inherited_scripts: false,
        };
        self.copy_script_lines_to(&mut cloned);
        cloned.core.merge(&self.core);
        cloned
    }

    /// Absorbs another level's system options (`Option::Merge`
    /// for scalars, `MergeOrShare` for domain groups): the source's
    /// non-empty paths win, explicitly-set scalars transfer with
    /// their flags, and domain rules append.
    pub fn merge_system(&mut self, src: &NgxOptions) {
        merge_system_str(
            &mut self.system.file_cache_path,
            &src.system.file_cache_path,
        );
        merge_system_str(&mut self.system.log_dir, &src.system.log_dir);
        merge_system_str(
            &mut self.system.statistics_path,
            &src.system.statistics_path,
        );
        merge_system_str(
            &mut self.system.global_statistics_path,
            &src.system.global_statistics_path,
        );
        merge_system_str(&mut self.system.console_path, &src.system.console_path);
        merge_system_str(&mut self.system.messages_path, &src.system.messages_path);
        merge_system_str(&mut self.system.admin_path, &src.system.admin_path);
        merge_system_str(
            &mut self.system.global_admin_path,
            &src.system.global_admin_path,
        );
        merge_system_str(&mut self.system.purge_method, &src.system.purge_method);
        // Explicitly-set scalars transfer with their flags.
        macro_rules! flagged {
            ($field:ident, $flag:ident) => {
                if src.system.was_set.$flag {
                    self.system.$field = src.system.$field.clone();
                    self.system.was_set.$flag = true;
                }
            };
        }
        flagged!(statistics_enabled, statistics_enabled);
        flagged!(statistics_logging_enabled, statistics_logging_enabled);
        flagged!(
            statistics_logging_interval_ms,
            statistics_logging_interval_ms
        );
        flagged!(
            statistics_logging_max_file_size_kb,
            statistics_logging_max_file_size_kb
        );
        flagged!(fetcher_proxy, fetcher_proxy);
        flagged!(fetch_with_gzip, fetch_with_gzip);
        flagged!(blocking_fetch_timeout_ms, blocking_fetch_timeout_ms);
        flagged!(file_cache_clean_interval_ms, clean_interval);
        flagged!(file_cache_clean_size_kb, clean_size);
        flagged!(file_cache_clean_inode_limit, clean_inode);
        flagged!(lru_cache_byte_limit, lru_byte_limit);
        flagged!(lru_cache_kb_per_process, lru_kb_per_process);
        flagged!(cache_flush_filename, flush_filename);
        flagged!(compress_metadata_cache, compress_metadata);
        flagged!(ipro_max_response_bytes, ipro_max_response_bytes);
        flagged!(
            ipro_max_concurrent_recordings,
            ipro_max_concurrent_recordings
        );
        // Server lists replace when present (explicit-clear is
        // unrepresentable, matching the path convention).
        if !src.system.memcached_servers.is_empty() {
            self.system
                .memcached_servers
                .clone_from(&src.system.memcached_servers);
        }
        if src.system.redis_server.is_some() {
            self.system
                .redis_server
                .clone_from(&src.system.redis_server);
        }
        self.system
            .statistics_domains
            .append_from(&src.system.statistics_domains);
        self.system
            .global_statistics_domains
            .append_from(&src.system.global_statistics_domains);
        self.system
            .messages_domains
            .append_from(&src.system.messages_domains);
        self.system
            .console_domains
            .append_from(&src.system.console_domains);
        self.system
            .admin_domains
            .append_from(&src.system.admin_domains);
        self.system
            .global_admin_domains
            .append_from(&src.system.global_admin_domains);
    }

    /// Replaces the destination's script lines (`CopyScriptLinesTo`).
    pub fn copy_script_lines_to(&self, dest: &mut NgxOptions) {
        dest.script_lines = self.script_lines.clone();
    }

    /// Appends script lines preserving order (`AppendScriptLinesTo`).
    pub fn append_script_lines_to(&self, dest: &mut NgxOptions) {
        dest.script_lines.extend(self.script_lines.iter().cloned());
    }

    /// Fully passive (`unplugged()` on the core options).
    #[must_use]
    pub fn unplugged(&self) -> bool {
        self.core.unplugged()
    }

    /// Idle-but-serving (`standby()` on the core options).
    #[must_use]
    pub fn standby(&self) -> bool {
        self.core.standby()
    }
}

/// Scalar system-option merge (`Option<T>::Merge`): the source's
/// non-empty value wins (explicit empty-to-clear is unrepresentable
/// here, matching the Slice 1 convention for the path options).
fn merge_system_str(dest: &mut String, src: &str) {
    if !src.is_empty() {
        *dest = src.to_string();
    }
}

/// Order-sensitive option merge (`ps_merge_options`).
///
/// Nothing when the parent is absent; a clone when the child is;
/// otherwise the child is rebased onto a clone of the parent so
/// its own directives win, then script lines are copied (when
/// the child clears inherited scripts) or appended.
pub fn merge_options(parent: Option<&NgxOptions>, child: &mut Option<NgxOptions>) {
    let Some(parent) = parent else {
        return;
    };
    if child.is_none() {
        *child = Some(parent.clone_with());
        return;
    }
    let child_specific = child.take().expect("checked above");
    let mut rebased = parent.clone_with();
    rebased.core.merge(&child_specific.core);
    rebased.merge_system(&child_specific);
    if child_specific.clear_inherited_scripts {
        child_specific.copy_script_lines_to(&mut rebased);
    } else {
        child_specific.append_script_lines_to(&mut rebased);
    }
    *child = Some(rebased);
}

/// Dummy hostname for server contexts (`MakeNgxServerContext`).
pub const DUMMY_HOSTNAME: &str = "dummy_hostname";

/// Dummy port: the negated server-context count (`-count`,
/// counted after incrementing).
#[must_use]
pub const fn dummy_port(server_context_count: u32) -> i32 {
    -(server_context_count as i32)
}

/// FileCachePath validation (`ps_merge_srv_conf` tail).
///
/// Texts are verbatim, including the standby pair, which reads
/// swapped (normal mode is told the path "must be set, even for
/// standby" while standby mode gets the plain message).
pub fn validate_file_cache_path(
    path: &str,
    unplugged: bool,
    standby: bool,
    is_dir: impl Fn(&str) -> bool,
) -> Result<(), &'static str> {
    if unplugged {
        return Ok(());
    }
    if path.is_empty() {
        if standby {
            return Err("FileCachePath must be set");
        }
        return Err("FileCachePath must be set, even for standby");
    }
    if !is_dir(path) {
        return Err("FileCachePath must be an nginx-writeable directory");
    }
    Ok(())
}

/// Server context (`NgxServerContext` without its factory half).
///
/// Carries the config-time identity (dummy hostname/port from
/// `MakeNgxServerContext`), the merged global options, the
/// `http2` variable index, and the child-init latch the content
/// handler gates on. Statistics, timers, and the request-context
/// constructor ride the driver factory. Not `Clone`: contexts own
/// live caches.
pub struct ServerContext {
    /// Dummy hostname (always `DUMMY_HOSTNAME`).
    pub hostname: String,
    /// Dummy port (negated context count).
    pub dummy_port: i32,
    /// Merged global options.
    pub options: NgxOptions,
    /// `http2` variable index (`NGX_ERROR` when unknown).
    pub http2_variable_index: ngx_int_t,
    /// Child-initialized latch (`initialized()`).
    pub initialized: bool,
    /// Server message handler (`SetServerContextMessageHandler`;
    /// attached at child init from the server's error log).
    pub message_handler: Option<NgxMessageHandler>,
    /// HTTP cache (`SetupCaches`; attached at child init).
    pub http_cache: Option<ps_http::http_cache::HttpCache>,
    /// Metadata cache (same source).
    pub metadata_cache: Option<std::sync::Arc<dyn ps_cache::cache::Cache>>,
    /// HTML rewrite context (same source; shares caches, clock
    /// reset per request).
    pub html_context: Option<std::rc::Rc<ps_html::server_context::ServerContext>>,
}

impl std::fmt::Debug for ServerContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerContext")
            .field("hostname", &self.hostname)
            .field("initialized", &self.initialized)
            .field("html_context", &self.html_context.is_some())
            .finish_non_exhaustive()
    }
}

impl ServerContext {
    /// Nginx options (`config()`: the `NgxRewriteOptions` view;
    /// the same object `global_options()` returns in C++).
    #[must_use]
    pub fn config(&self) -> &NgxOptions {
        &self.options
    }

    /// Whether child init ran (`initialized()`): a context from a
    /// refused reload cycle serves nothing, beacons included.
    #[must_use]
    pub fn initialized(&self) -> bool {
        self.initialized
    }

    /// Polls for cache flush (`FlushCacheIfNecessary`): rate-limited
    /// inside, so the content handler calls it on every request.
    /// No-op until Slice 6 wires the cache state behind it.
    pub fn poll_cache_flush(&self) {}

    /// Formats an option line (`FormatOption`).
    #[must_use]
    pub fn format_option(option_name: &str, args: &str) -> String {
        format!("pagespeed {option_name} {args};")
    }
}

/// ProxyFetch never fetches HTML via mapped domains here
/// (`ProxiesHtml()`, always false).
pub const PROXIES_HTML: bool = false;

/// Main conf (`ps_main_conf_t`).
#[derive(Debug, Default)]
pub struct MainConf {
    /// Factory slot: live generation plus the deleted-once latch
    /// (`driver_factory`, `factory_deleted`).
    pub factory: FactorySlot,
    /// Server-context counter (`server_context_count`).
    pub server_context_count: u32,
    /// Driver factory (`driver_factory`, created eagerly).
    pub driver_factory: DriverFactory,
    /// Base-fetch pipe read connection (registered at child
    /// init, closed through nginx at child exit).
    pub base_fetch_read_conn: Option<*mut ngx::ffi::ngx_connection_t>,
}

/// Server conf (`ps_srv_conf_t`).
#[derive(Debug, Default)]
pub struct SrvConf {
    /// Level options (moved into the context at merge).
    pub options: Option<NgxOptions>,
    /// Server context after merge (`server_context`).
    pub context: Option<ServerContext>,
}

impl SrvConf {
    /// Fully passive server (`ps_disabled`): no context (pagespeed
    /// configured elsewhere but not here), or an unplugged one.
    #[must_use]
    pub fn disabled(&self) -> bool {
        self.context
            .as_ref()
            .is_none_or(|ctx| ctx.options.unplugged())
    }
}

/// Location conf (`ps_loc_conf_t`).
#[derive(Debug, Default)]
pub struct LocConf {
    /// Level options.
    pub options: Option<NgxOptions>,
}

/// Cycle factory slot (`driver_factory` + `factory_deleted`).
#[derive(Debug, Clone, Default)]
pub struct FactorySlot {
    /// Live factory generation in this cycle, if any.
    pub generation: Option<u64>,
    /// Whether this cycle already deleted its factory.
    pub deleted: bool,
}

/// Deletes the cycle's factory once (`ps_delete_cycle_factory`).
///
/// `active` is the tracked live generation; it is cleared only
/// when it points at this very factory, so a discarded cycle
/// never untracks the still-live one.
pub fn delete_cycle_factory(slot: &mut FactorySlot, active: &mut Option<u64>) {
    if slot.deleted || slot.generation.is_none() {
        return;
    }
    let generation = slot.generation;
    if *active == generation {
        *active = None;
    }
    slot.generation = None;
    slot.deleted = true;
}

/// Merges main options into a server conf (`ps_merge_srv_conf`).
///
/// Counts the server, rebases options, builds the context with
/// the dummy identity, and validates the cache path. `is_dir`
/// answers the directory check (live filesystem at runtime).
/// `http2_index` is the `http2` variable lookup the C++ runs
/// conditionally at this call site; the index travels on the
/// context for request-time use.
pub fn merge_srv_conf(
    main: &mut MainConf,
    parent_options: Option<&NgxOptions>,
    srv: &mut SrvConf,
    http2_index: ngx_int_t,
    is_dir: impl Fn(&str) -> bool,
) -> Result<ngx_int_t, &'static str> {
    main.server_context_count += 1;
    merge_options(parent_options, &mut srv.options);
    let Some(options) = srv.options.take() else {
        return Ok(http2_index);
    };
    // Process-scope main options propagate to the factory
    // defaults (`SetMainConf`); a null parent skips.
    main.driver_factory.set_main_conf(parent_options);
    // The context applies its own defaults before the file
    // options merge in; then the file options are done. The
    // base comes from the factory (`NewRewriteOptions`, with
    // its explicit CoreFilters level), not a blank.
    let mut global = main.driver_factory.new_options();
    global.core.merge(&options.core);
    global.merge_system(&options);
    options.copy_script_lines_to(&mut global);
    validate_file_cache_path(
        &global.system.file_cache_path,
        global.unplugged(),
        global.standby(),
        is_dir,
    )?;
    srv.context = Some(ServerContext {
        hostname: DUMMY_HOSTNAME.to_string(),
        dummy_port: dummy_port(main.server_context_count),
        options: global,
        http2_variable_index: http2_index,
        // Child init (Slice 6) latches this on.
        initialized: false,
        // Attached at child init from the server's error log.
        message_handler: None,
        // Assembled at child init from the registered paths.
        http_cache: None,
        metadata_cache: None,
        html_context: None,
    });
    Ok(http2_index)
}

/// Merges location options (`ps_merge_loc_conf`).
///
/// Four cases: no directives anywhere (inherit enclosing or
/// nothing), nested rebase off the enclosing location, or
/// rebase off the server's globals — unless the server is
/// disabled, in which case location-only directives are
/// silently dropped.
pub fn merge_loc_conf(
    parent: Option<&NgxOptions>,
    loc: &mut LocConf,
    server: Option<&NgxOptions>,
    server_disabled: bool,
) {
    if loc.options.is_none() {
        if let Some(parent) = parent {
            loc.options = Some(parent.clone_with());
        }
        return;
    }
    if parent.is_some() {
        merge_options(parent, &mut loc.options);
        return;
    }
    if server_disabled {
        return;
    }
    merge_options(server, &mut loc.options);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps_config::options::{apply_nginx_directive, NginxFactoryOptions};
    use ps_config::scope::Scope;

    fn ngx(directives: &[(&str, &[&str])]) -> NgxOptions {
        let mut options = NgxOptions::new();
        let mut factory = NginxFactoryOptions::default();
        for (directive, args) in directives {
            if directive.eq_ignore_ascii_case("ClearInheritedScripts") && args.is_empty() {
                options.clear_inherited_scripts = true;
            }
            apply_nginx_directive(
                &mut options.core,
                &mut factory,
                &mut options.system,
                directive,
                args,
                Scope::Server,
            )
            .unwrap();
            if !args.is_empty() || !directive.eq_ignore_ascii_case("ClearInheritedScripts") {
                options.script_lines.push(ScriptLine {
                    args: std::iter::once(directive.to_string())
                        .chain(args.iter().map(|s| s.to_string()))
                        .collect(),
                    scope: Scope::Server,
                });
            }
        }
        options
    }

    #[test]
    fn system_scalars_merge_when_set() {
        // The accumulator starts as the parent; the narrower
        // child's explicitly-set values win.
        let mut parent = NgxOptions::new();
        parent.system.file_cache_clean_size_kb = 100;
        parent.system.was_set.clean_size = true;
        let child = NgxOptions::new();
        let mut merged = parent.clone();
        merged.merge_system(&child);
        assert_eq!(100, merged.system.file_cache_clean_size_kb);
        assert!(merged.system.was_set.clean_size);
        // A flagged child value transfers...
        let mut child = NgxOptions::new();
        child.system.compress_metadata_cache = false;
        child.system.was_set.compress_metadata = true;
        merged.merge_system(&child);
        assert!(!merged.system.compress_metadata_cache);
        assert!(merged.system.was_set.compress_metadata);
        // ... but an explicit child default does override.
        let mut child = NgxOptions::new();
        child.system.was_set.clean_size = true;
        merged.merge_system(&child);
        assert_eq!(102_400, merged.system.file_cache_clean_size_kb);
        // Recording caps transfer with their flags.
        let mut child = NgxOptions::new();
        child.system.ipro_max_response_bytes = 1024;
        child.system.was_set.ipro_max_response_bytes = true;
        child.system.ipro_max_concurrent_recordings = 3;
        child.system.was_set.ipro_max_concurrent_recordings = true;
        merged.merge_system(&child);
        assert_eq!(1024, merged.system.ipro_max_response_bytes);
        assert!(merged.system.was_set.ipro_max_response_bytes);
        assert_eq!(3, merged.system.ipro_max_concurrent_recordings);
        assert!(merged.system.was_set.ipro_max_concurrent_recordings);
    }

    #[test]
    fn merge_options_order() {
        // Absent parent: nothing.
        let mut child = Some(ngx(&[("RewriteLevel", &["CoreFilters"])]));
        merge_options(None, &mut child);
        assert!(child.is_some());
        // Absent child: clone.
        let parent = ngx(&[("RewriteLevel", &["CoreFilters"])]);
        let mut child: Option<NgxOptions> = None;
        merge_options(Some(&parent), &mut child);
        assert!(child.is_some());
        // Both: child directives win (PassThrough parent,
        // CoreFilters child stays CoreFilters).
        let parent = ngx(&[("RewriteLevel", &["PassThrough"])]);
        let mut child = Some(ngx(&[("RewriteLevel", &["CoreFilters"])]));
        merge_options(Some(&parent), &mut child);
        let merged = child.unwrap();
        assert_eq!(
            ps_config::filter::RewriteLevel::CoreFilters,
            merged.core.filters().level()
        );
        // Script lines append in order by default.
        assert_eq!(2, merged.script_lines.len());
        assert_eq!("PassThrough", merged.script_lines[0].args[1]);
        assert_eq!("CoreFilters", merged.script_lines[1].args[1]);
    }

    #[test]
    fn merge_options_clear_copies() {
        let parent = ngx(&[("RewriteLevel", &["PassThrough"])]);
        let mut child = Some(ngx(&[
            ("ClearInheritedScripts", &[]),
            ("RewriteLevel", &["CoreFilters"]),
        ]));
        merge_options(Some(&parent), &mut child);
        let merged = child.unwrap();
        assert_eq!(1, merged.script_lines.len());
        assert_eq!("CoreFilters", merged.script_lines[0].args[1]);
    }

    #[test]
    fn clone_resets_clear_flag() {
        let options = ngx(&[("ClearInheritedScripts", &[])]);
        assert!(options.clear_inherited_scripts);
        assert!(!options.clone_with().clear_inherited_scripts);
    }

    #[test]
    fn dummy_ports_descend() {
        assert_eq!(-1, dummy_port(1));
        assert_eq!(-2, dummy_port(2));
    }

    #[test]
    fn cache_path_validation_texts() {
        // Unplugged skips validation entirely.
        assert!(validate_file_cache_path("", true, false, |_| false).is_ok());
        // Empty path: standby and normal texts, verbatim.
        assert_eq!(
            Err("FileCachePath must be set"),
            validate_file_cache_path("", false, true, |_| true)
        );
        assert_eq!(
            Err("FileCachePath must be set, even for standby"),
            validate_file_cache_path("", false, false, |_| true)
        );
        // Present but not a directory.
        assert_eq!(
            Err("FileCachePath must be an nginx-writeable directory"),
            validate_file_cache_path("/nope", false, false, |_| false)
        );
        assert!(validate_file_cache_path("/ok", false, false, |_| true).is_ok());
    }

    #[test]
    fn srv_merge_seeds_context() {
        let mut main = MainConf::default();
        let parent = ngx(&[("RewriteLevel", &["PassThrough"])]);
        let mut srv = SrvConf {
            options: Some(ngx(&[
                ("RewriteLevel", &["CoreFilters"]),
                ("FileCachePath", &["/cache"]),
            ])),
            context: None,
        };
        let index = merge_srv_conf(&mut main, Some(&parent), &mut srv, 7, |_| true).unwrap();
        assert_eq!(7, index);
        assert_eq!(1, main.server_context_count);
        // Options moved into the context.
        assert!(srv.options.is_none());
        let ctx = srv.context.unwrap();
        assert_eq!("dummy_hostname", ctx.hostname);
        assert_eq!(-1, ctx.dummy_port);
        assert_eq!("/cache", ctx.options.system.file_cache_path);
        assert_eq!(7, ctx.http2_variable_index);
        assert!(!ctx.initialized());
        // Without options: counted but untouched.
        let mut srv = SrvConf::default();
        merge_srv_conf(&mut main, None, &mut srv, 7, |_| true).unwrap();
        assert_eq!(2, main.server_context_count);
        assert!(srv.context.is_none());
    }

    #[test]
    fn srv_merge_propagates_main_conf() {
        use ps_config::scope::Scope;
        let mut main = MainConf::default();
        let mut parent = NgxOptions::new();
        parent
            .core
            .set_option("ImageMaxRewritesAtOnce", "4", Scope::ProcessStrict)
            .unwrap();
        let mut srv = SrvConf {
            options: Some(ngx(&[("FileCachePath", &["/cache"])])),
            context: None,
        };
        merge_srv_conf(&mut main, Some(&parent), &mut srv, 0, |_| true).unwrap();
        assert!(main.driver_factory.default_options.core.was_set("im"));
        // A null parent skips without clearing.
        let mut srv = SrvConf {
            options: Some(ngx(&[("FileCachePath", &["/cache"])])),
            context: None,
        };
        merge_srv_conf(&mut main, None, &mut srv, 0, |_| true).unwrap();
        assert!(main.driver_factory.default_options.core.was_set("im"));
    }

    #[test]
    fn srv_merge_rejects_missing_cache_path() {
        let mut main = MainConf::default();
        let mut srv = SrvConf {
            options: Some(ngx(&[("RewriteLevel", &["CoreFilters"])])),
            context: None,
        };
        assert_eq!(
            Err("FileCachePath must be set, even for standby"),
            merge_srv_conf(&mut main, None, &mut srv, 0, |_| true)
        );
    }

    #[test]
    fn loc_merge_four_cases() {
        // 1. Nothing anywhere: stays empty.
        let mut loc = LocConf::default();
        merge_loc_conf(None, &mut loc, None, false);
        assert!(loc.options.is_none());
        // 2. Enclosing location inherits down.
        let parent = ngx(&[("RewriteLevel", &["CoreFilters"])]);
        merge_loc_conf(Some(&parent), &mut loc, None, false);
        assert!(loc.options.is_some());
        // 3. Nested rebase: own directives win.
        let mut loc = LocConf {
            options: Some(ngx(&[("RewriteLevel", &["CoreFilters"])])),
        };
        let parent = ngx(&[("RewriteLevel", &["PassThrough"])]);
        merge_loc_conf(Some(&parent), &mut loc, None, false);
        assert_eq!(
            ps_config::filter::RewriteLevel::CoreFilters,
            loc.options.unwrap().core.filters().level()
        );
        // 4a. No enclosing options + disabled server: dropped.
        let mut loc = LocConf {
            options: Some(ngx(&[("RewriteLevel", &["CoreFilters"])])),
        };
        merge_loc_conf(None, &mut loc, None, true);
        assert!(loc.options.is_some());
        // 4b. No enclosing options + live server: rebase off globals.
        let server = ngx(&[("FileCachePath", &["/srv-cache"])]);
        merge_loc_conf(None, &mut loc, Some(&server), false);
        let merged = loc.options.unwrap();
        assert_eq!("/srv-cache", merged.system.file_cache_path);
    }

    #[test]
    fn system_merge_paths_and_domains() {
        let mut parent = NgxOptions::new();
        parent.system.admin_path = "/parent_admin".to_string();
        parent.system.purge_method = "PURGE".to_string();
        parent.system.admin_domains.allow("*.example.com");
        let mut child = Some(NgxOptions::new());
        child.as_mut().unwrap().system.admin_path = "/child_admin".to_string();
        child.as_mut().unwrap().system.statistics_path = "/stats".to_string();
        child
            .as_mut()
            .unwrap()
            .system
            .admin_domains
            .disallow("evil.example.com");
        merge_options(Some(&parent), &mut child);
        let merged = child.unwrap();
        // Child paths win; parent-only values survive.
        assert_eq!("/child_admin", merged.system.admin_path);
        assert_eq!("/stats", merged.system.statistics_path);
        assert_eq!("PURGE", merged.system.purge_method);
        // Domain rules union: last-inserted matching rule wins.
        assert!(merged
            .system
            .admin_domains
            .is_match("www.example.com", false));
        assert!(!merged
            .system
            .admin_domains
            .is_match("evil.example.com", false));
        assert!(!merged.system.admin_domains.is_match("other.org", false));
    }

    #[test]
    fn server_disabled_and_format() {
        // No context: disabled.
        let srv = SrvConf::default();
        assert!(srv.disabled());
        // Live context: enabled.
        let mut main = MainConf::default();
        let mut srv = SrvConf {
            options: Some(ngx(&[
                ("RewriteLevel", &["CoreFilters"]),
                ("FileCachePath", &["/cache"]),
            ])),
            context: None,
        };
        merge_srv_conf(&mut main, None, &mut srv, 0, |_| true).unwrap();
        assert!(!srv.disabled());
        let ctx = srv.context.as_ref().unwrap();
        assert_eq!("/cache", ctx.config().system.file_cache_path);
        assert_eq!(
            "pagespeed RewriteLevel CoreFilters;",
            ServerContext::format_option("RewriteLevel", "CoreFilters")
        );
        // Unplugged context: disabled.
        let mut srv = SrvConf {
            options: Some(ngx(&[("off", &[]), ("FileCachePath", &["/cache"])])),
            context: None,
        };
        merge_srv_conf(&mut main, None, &mut srv, 0, |_| true).unwrap();
        assert!(srv.disabled());
    }

    #[test]
    fn factory_delete_guards() {
        let mut slot = FactorySlot {
            generation: Some(3),
            deleted: false,
        };
        let mut active = Some(3);
        delete_cycle_factory(&mut slot, &mut active);
        assert_eq!(None, slot.generation);
        assert!(slot.deleted);
        assert_eq!(None, active);
        // Second call is a no-op.
        delete_cycle_factory(&mut slot, &mut active);
        // A foreign active generation is never untracked.
        let mut slot = FactorySlot {
            generation: Some(4),
            deleted: false,
        };
        let mut active = Some(9);
        delete_cycle_factory(&mut slot, &mut active);
        assert_eq!(Some(9), active);
        assert!(slot.deleted);
    }
}
