//! Driver factory (`NgxRewriteDriverFactory` config half).
//!
//! Ports the factory's configuration surface: the constructor
//! defaults, the system factory directives (the nginx-native
//! three live in ps-config's `apply_nginx_directive`),
//! post-configuration validation, the scheduler thread,
//! shutdown sequencing, and the remote-config fetcher warnings.
//! Fetchers, caches, the message ring, and the live statistics
//! backend join in later Slice 6 turns.

use crate::caches::SystemCaches;
use crate::conf::NgxOptions;
use crate::message::NgxMessageHandler;
use crate::native_fetch::{allocate_fetcher, fetcher_key, FetchId, FetcherKeyInput, NativeFetcher};
use crate::scheduler::Scheduler;
use ngx::ffi::ngx_log_t;
use ps_config::options::{parse_bool, parse_int32, parse_int64, NginxFactoryOptions};
use ps_config::scope::Scope;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread::JoinHandle;

/// Unset resolver timeout (`NGX_CONF_UNSET_MSEC`).
pub const UNSET_MSEC: i64 = -1;

/// Default resolver timeout once a resolver is installed (ms).
pub const DEFAULT_RESOLVER_TIMEOUT_MS: i64 = 1000;

/// Default static-asset prefix.
pub const DEFAULT_STATIC_ASSET_PREFIX: &str = "/pagespeed_static/";

/// Default nginx beacon endpoint (factory default, Slice 2 pin).
pub const DEFAULT_NGINX_BEACON_URL: &str = "/ngx_pagespeed_beacon";

/// Default file-cache clean inode limit.
pub const DEFAULT_FILE_CACHE_CLEAN_INODE_LIMIT: i64 = 500000;

/// Default shared fetcher count (`AutoDetectThreadCounts` fills
/// thread counts; the fetcher count is fixed).
pub const NUM_SHARED_FETCHERS: i32 = 32;

/// Scheduler-thread ownership (`SchedulerThread` plus its deleter).
pub struct SchedulerThreadState {
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for SchedulerThreadState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SchedulerThreadState")
            .finish_non_exhaustive()
    }
}

impl SchedulerThreadState {
    fn new() -> Self {
        SchedulerThreadState { thread: None }
    }
}

/// Driver factory (`NgxRewriteDriverFactory` config half).
#[derive(Debug)]
pub struct DriverFactory {
    /// Identity for logs (`hostname:port`).
    pub hostname: String,
    /// Identity port (unused, like C++).
    pub port: i32,
    /// Default options (factory ctor applications).
    pub default_options: NgxOptions,
    /// Factory-level options (fetchers, resolver, script mode).
    pub factory_options: NginxFactoryOptions,
    /// Static-asset URL prefix.
    pub static_asset_prefix: String,
    /// Whether a resolver is configured (`resolver_ != NULL`).
    pub resolver_present: bool,
    /// Resolver timeout in ms (`UNSET_MSEC` until installed).
    pub resolver_timeout_ms: i64,
    /// Per-vhost statistics.
    pub use_per_vhost_statistics: bool,
    /// Install the crash handler at logging init.
    pub install_crash_handler: bool,
    /// Message-buffer capacity in bytes (0 disables).
    pub message_buffer_size: i64,
    /// Track original content lengths.
    pub track_original_content_length: bool,
    /// List outstanding URLs on fetch errors.
    pub list_outstanding_urls_on_error: bool,
    /// Force caching for every response.
    pub force_caching: bool,
    /// Rewrite thread count (-1 autodetects).
    pub num_rewrite_threads: i32,
    /// Expensive-rewrite thread count (-1 autodetects).
    pub num_expensive_rewrite_threads: i32,
    /// Thread counts finalized (`AutoDetectThreadCounts` ran).
    pub thread_counts_finalized: bool,
    /// Threads started (workers only).
    pub threads_started: bool,
    /// Shut down (idempotent latch).
    pub shut_down: bool,
    /// Factory message handler (`ngx_message_handler_`).
    pub message_handler: NgxMessageHandler,
    /// HTML-parse message handler.
    pub html_parse_message_handler: NgxMessageHandler,
    /// Alarm scheduler (created eagerly; C++ builds it lazily
    /// with no observable difference).
    pub scheduler: Arc<Scheduler>,
    /// Scheduler-thread ownership.
    pub scheduler_thread: SchedulerThreadState,
    /// Fetchers by config key (`fetcher_map_`/`base_fetcher_map_`
    /// collapsed: slurping is unsupported so both keys agree).
    pub fetchers: HashMap<String, NativeFetcher>,
    /// Cache system (paths registered at init).
    pub caches: SystemCaches,
    /// Worker statistics registry (cross-process shared counters
    /// once `init_module` swaps in the mapped table; the null
    /// registry built here only serves pre-config callers).
    pub statistics: Arc<ps_stats::core::Statistics>,
    /// Shared message buffer (ring stand-in; disabled until
    /// `MessageBufferSize` sets a budget).
    pub message_buffer: Arc<Mutex<ps_admin::log_buffer::LogBuffer>>,
    /// Shared purge set (admin writes land here; fetch-flow
    /// validity checks are a follow-up).
    pub purge_set: ps_cache::purge::PurgeSet,
}

impl DriverFactory {
    /// Creates a factory (ctor): blank hostname/port defaults,
    /// beacon URL, inode limit, and introspective-JS renaming on
    /// the default options.
    #[must_use]
    pub fn new(hostname: &str, port: i32) -> Self {
        let mut default_options = NgxOptions::new();
        default_options
            .core
            .set_option("BeaconUrl", DEFAULT_NGINX_BEACON_URL, Scope::ProcessStrict)
            .expect("literal beacon parses");
        default_options
            .core
            .set_avoid_renaming_introspective_javascript(true);
        // `set_default_x_header_value(kModPagespeedVersion)`: a
        // base value here behaves as the default (file options
        // merge over it).
        default_options
            .core
            .set_option(
                "XHeaderValue",
                crate::message::MODULE_VERSION,
                Scope::ProcessStrict,
            )
            .expect("literal version parses");
        default_options.system.file_cache_clean_inode_limit = DEFAULT_FILE_CACHE_CLEAN_INODE_LIMIT;
        DriverFactory {
            hostname: hostname.to_string(),
            port,
            default_options,
            factory_options: NginxFactoryOptions::default(),
            static_asset_prefix: DEFAULT_STATIC_ASSET_PREFIX.to_string(),
            resolver_present: false,
            resolver_timeout_ms: UNSET_MSEC,
            use_per_vhost_statistics: true,
            install_crash_handler: false,
            message_buffer_size: 0,
            track_original_content_length: false,
            list_outstanding_urls_on_error: false,
            force_caching: false,
            num_rewrite_threads: -1,
            num_expensive_rewrite_threads: -1,
            thread_counts_finalized: false,
            threads_started: false,
            shut_down: false,
            message_handler: NgxMessageHandler::new(),
            html_parse_message_handler: NgxMessageHandler::new(),
            scheduler: Arc::new(Scheduler::new()),
            scheduler_thread: SchedulerThreadState::new(),
            fetchers: HashMap::new(),
            caches: SystemCaches::new(),
            statistics: Arc::new(ps_stats::core::Statistics::new()),
            message_buffer: Arc::new(Mutex::new(ps_admin::log_buffer::LogBuffer::disabled())),
            purge_set: ps_cache::purge::PurgeSet::new(25_000),
        }
    }

    /// Registers every context's config and emits the root-phase
    /// lines (`RootInit`: history archive, controller startup,
    /// and shm setup skipped without their backends).
    pub fn root_init(&mut self, contexts: &[&NgxOptions]) -> Vec<String> {
        let now_ms = self.scheduler.now_us() / 1000;
        let mut messages = Vec::new();
        for options in contexts {
            messages.extend(self.caches.register_config(
                &options.system,
                options.core.enable_cache_purge(),
                options.unplugged(),
                now_ms,
            ));
        }
        messages.extend(self.caches.root_init_messages());
        messages
    }

    /// Emits the child-phase lines (`ChildInit` cache half).
    #[must_use]
    pub fn child_init_caches(&self) -> Vec<String> {
        self.caches.child_init_messages()
    }

    /// Looks up or creates the fetcher for a config
    /// (`GetFetcher`/`GetBaseFetcher`): returns the map key plus
    /// any fallback warning (warned once, on creation only).
    pub fn get_fetcher(&mut self, options: &NgxOptions) -> (String, Option<String>) {
        let key = fetcher_key(&FetcherKeyInput {
            unplugged: options.unplugged(),
            list_errors: self.list_outstanding_urls_on_error,
            proxy: &options.system.fetcher_proxy,
            gzip: options.system.fetch_with_gzip,
            track_content_length: self.track_original_content_length,
            timeout_ms: options.system.blocking_fetch_timeout_ms,
        });
        if self.fetchers.contains_key(&key) {
            return (key, None);
        }
        let warning = allocate_fetcher(self.factory_options.use_native_fetcher);
        self.fetchers.insert(key.clone(), NativeFetcher::new());
        (key, warning)
    }

    /// Borrows a mapped fetcher.
    #[must_use]
    pub fn fetcher(&self, key: &str) -> Option<&NativeFetcher> {
        self.fetchers.get(key)
    }

    /// Mutably borrows a mapped fetcher.
    pub fn fetcher_mut(&mut self, key: &str) -> Option<&mut NativeFetcher> {
        self.fetchers.get_mut(key)
    }

    /// Starts the scheduler thread (`StartThreads`, idempotent).
    pub fn start_threads(&mut self) {
        if self.threads_started {
            return;
        }
        let scheduler = Arc::clone(&self.scheduler);
        let thread = std::thread::Builder::new()
            .name("scheduler_thread".to_string())
            .spawn(move || scheduler.pump_until_stopped())
            .expect("Unable to start scheduler thread");
        self.scheduler_thread.thread = Some(thread);
        self.threads_started = true;
    }

    /// Shares the alarm scheduler (`scheduler()`).
    #[must_use]
    pub fn scheduler(&self) -> Arc<Scheduler> {
        Arc::clone(&self.scheduler)
    }

    /// Stops the scheduler thread (the deleter: quit, wake, join).
    /// A failed join is ignored so shutdown always completes.
    fn stop_scheduler_thread(&mut self) {
        self.scheduler.request_stop();
        if let Some(thread) = self.scheduler_thread.thread.take() {
            let _ = thread.join();
        }
    }

    /// Fresh driver options (`NewRewriteOptions`): blank with an
    /// explicit CoreFilters level.
    #[must_use]
    pub fn new_options(&self) -> NgxOptions {
        let mut options = NgxOptions::new();
        options
            .core
            .set_rewrite_level(ps_config::filter::RewriteLevel::CoreFilters);
        options
            .core
            .set_option(
                "XHeaderValue",
                crate::message::MODULE_VERSION,
                ps_config::scope::Scope::ProcessStrict,
            )
            .expect("literal version parses");
        options
    }

    /// Merges process-scope main options into the defaults
    /// (`SetMainConf`).
    pub fn set_main_conf(&mut self, main_options: Option<&NgxOptions>) {
        if let Some(main_options) = main_options {
            self.default_options
                .core
                .merge_process_scope_only(&main_options.core);
        }
    }

    /// Records the configured resolver (`set_resolver`).
    pub fn set_resolver(&mut self, present: bool) {
        self.resolver_present = present;
    }

    /// Records the resolver timeout, defaulting an unset wait
    /// (`set_resolver_timeout`).
    pub fn set_resolver_timeout(&mut self, timeout_ms: i64) {
        self.resolver_timeout_ms = if timeout_ms == UNSET_MSEC {
            DEFAULT_RESOLVER_TIMEOUT_MS
        } else {
            timeout_ms
        };
    }

    /// Whether fetching can proceed (`CheckResolver`): the native
    /// fetcher needs a resolver; anything else passes.
    #[must_use]
    pub fn check_resolver(&self) -> bool {
        !(self.factory_options.use_native_fetcher && !self.resolver_present)
    }

    /// Finalizes thread counts (`AutoDetectThreadCounts`): nginx
    /// is non-threaded, so non-positive counts become 1. Returns
    /// the info text for the message handler; second calls are a
    /// no-op returning `None`. Thread startup itself is Slice 6b.
    pub fn finalize_thread_counts(&mut self) -> Option<String> {
        if self.thread_counts_finalized {
            return None;
        }
        if self.num_rewrite_threads <= 0 {
            self.num_rewrite_threads = 1;
        }
        if self.num_expensive_rewrite_threads <= 0 {
            self.num_expensive_rewrite_threads = 1;
        }
        self.thread_counts_finalized = true;
        Some(format!(
            "No threading detected. Own threads: {} Rewrite, {} Expensive Rewrite.",
            self.num_rewrite_threads, self.num_expensive_rewrite_threads
        ))
    }

    /// Finishes construction (`Init`): thread counts now; the
    /// shared caches join with their slice. Returns the
    /// autodetect info text, if any.
    pub fn init(&mut self) -> Option<String> {
        self.finalize_thread_counts()
    }

    /// Whether logging init installs the crash handler
    /// (`LoggingInit` gate).
    #[must_use]
    pub fn should_install_crash_handler(&self, may_install: bool) -> bool {
        may_install && self.install_crash_handler
    }

    /// Points logging at a cycle log (`LoggingInit`): installs the
    /// log bridge, the crash handler per the gate, both factory
    /// message handlers, and the shared message buffer.
    ///
    /// # Safety
    ///
    /// `log` must point at an `ngx_log_t` that stays live until
    /// process exit; installation is process global.
    pub unsafe fn logging_init(&mut self, log: *mut ngx_log_t, may_install_crash_handler: bool) {
        // SAFETY: caller guarantees a live log; the bridge only
        // reads its level and stores the pointer.
        unsafe {
            crate::log_bridge::install(log);
        }
        if self.should_install_crash_handler(may_install_crash_handler) {
            // SAFETY: same live log; installing handlers is the
            // documented process-global step.
            unsafe {
                crate::message::install_crash_handler(log);
            }
        }
        // SAFETY: same live log, outliving every message call.
        unsafe {
            self.message_handler.set_log(log);
            self.html_parse_message_handler.set_log(log);
        }
        self.message_handler
            .set_buffer(Arc::clone(&self.message_buffer));
        self.html_parse_message_handler
            .set_buffer(Arc::clone(&self.message_buffer));
    }

    /// Shuts down (`ShutDown`, idempotent): shuts every mapped
    /// fetcher and reports their pending/active task ids for the
    /// live driver to fail (Slice 7). Later turns slot cache,
    /// worker, and central-controller teardown around this call,
    /// in C++ order; the maps drop at factory destruction.
    pub fn shutdown(&mut self) -> Vec<(String, FetchId)> {
        if self.shut_down {
            return Vec::new();
        }
        self.shut_down = true;
        let mut fail = Vec::new();
        for (key, fetcher) in self.fetchers.iter_mut() {
            for task in fetcher.shutdown() {
                fail.push((key.clone(), task.id));
            }
            for id in fetcher.cancel_active() {
                fail.push((key.clone(), id));
            }
        }
        fail
    }
}

impl Default for DriverFactory {
    /// `ps_create_main_conf` creation args (verbatim: blank
    /// hostname, unused port).
    fn default() -> Self {
        DriverFactory::new("", -1)
    }
}

impl Drop for DriverFactory {
    /// Tears down (`~RewriteDriverFactory` order): shutdown latch
    /// first, then the scheduler join where the deferred
    /// cleanups run (context/worker/fetcher/cache teardown slots
    /// in between with their slices).
    fn drop(&mut self) {
        self.shutdown();
        self.stop_scheduler_thread();
    }
}

/// Factory directive outcome (`ParseAndSetOption{1,2}` result).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactoryDirectiveOutcome {
    /// Applied.
    Ok,
    /// Not a factory name (the caller reports unknown).
    NotFactory,
    /// Rejected: the result-switch detail (empty when C++ leaves
    /// `msg` empty; the caller wraps it with the full line).
    Invalid(String),
    /// Warned and ignored below top level (the warning text, also
    /// logged by the caller).
    WarnIgnored(String),
}

/// Message-buffer capacity limit (`CircularBuffer::kMaxCapacity`).
pub const MESSAGE_BUFFER_MAX: i32 = 1 << 30;

/// Applies a one-arg system factory directive
/// (`SystemRewriteDriverFactory::ParseAndSetOption1`).
///
/// Runs after the option table misses (the nginx-native three
/// never reach here: ps-config handles them first). Matching is
/// case-insensitive; scope echoes use the as-typed name. Failed
/// parses leave the field untouched: C++ writes the default and
/// returns invalid, but invalid fails config load, so the stray
/// write is unobservable.
pub fn apply_factory_directive_1(
    factory: &mut DriverFactory,
    directive: &str,
    arg: &str,
    scope: Scope,
) -> FactoryDirectiveOutcome {
    // The call site passes `scope >= kLegacyProcessScope`.
    let process_scope = scope >= Scope::LegacyProcess;
    // Hard-require top level (verbatim detail).
    macro_rules! require_global {
        () => {
            if !process_scope {
                return FactoryDirectiveOutcome::Invalid(format!(
                    "'{directive}' is global and can't be set at this scope."
                ));
            }
        };
    }
    // Warn and ignore below top level (verbatim warning).
    macro_rules! warn_unless_global {
        () => {
            if !process_scope {
                return FactoryDirectiveOutcome::WarnIgnored(format!(
                    "'{directive}' is global and is ignored at this scope"
                ));
            }
        };
    }
    if directive.eq_ignore_ascii_case("StaticAssetPrefix") {
        require_global!();
        factory.static_asset_prefix = arg.to_string();
        return FactoryDirectiveOutcome::Ok;
    }
    if directive.eq_ignore_ascii_case("UsePerVHostStatistics") {
        require_global!();
        return match parse_bool(arg) {
            Some(value) => {
                factory.use_per_vhost_statistics = value;
                FactoryDirectiveOutcome::Ok
            }
            None => FactoryDirectiveOutcome::Invalid(String::new()),
        };
    }
    if directive.eq_ignore_ascii_case("InstallCrashHandler") {
        require_global!();
        return match parse_bool(arg) {
            Some(value) => {
                factory.install_crash_handler = value;
                FactoryDirectiveOutcome::Ok
            }
            None => FactoryDirectiveOutcome::Invalid(String::new()),
        };
    }
    if directive.eq_ignore_ascii_case("NumRewriteThreads") {
        require_global!();
        return match parse_int32(arg) {
            Some(value) => {
                factory.num_rewrite_threads = value;
                FactoryDirectiveOutcome::Ok
            }
            None => FactoryDirectiveOutcome::Invalid(String::new()),
        };
    }
    if directive.eq_ignore_ascii_case("NumExpensiveRewriteThreads") {
        require_global!();
        return match parse_int32(arg) {
            Some(value) => {
                factory.num_expensive_rewrite_threads = value;
                FactoryDirectiveOutcome::Ok
            }
            None => FactoryDirectiveOutcome::Invalid(String::new()),
        };
    }
    if directive.eq_ignore_ascii_case("ForceCaching") {
        warn_unless_global!();
        return match parse_bool(arg) {
            Some(value) => {
                factory.force_caching = value;
                FactoryDirectiveOutcome::Ok
            }
            None => FactoryDirectiveOutcome::Invalid(String::new()),
        };
    }
    if directive.eq_ignore_ascii_case("ListOutstandingUrlsOnError") {
        warn_unless_global!();
        return match parse_bool(arg) {
            Some(value) => {
                factory.list_outstanding_urls_on_error = value;
                FactoryDirectiveOutcome::Ok
            }
            None => FactoryDirectiveOutcome::Invalid(String::new()),
        };
    }
    if directive.eq_ignore_ascii_case("MessageBufferSize") {
        warn_unless_global!();
        // The range check runs only when parsing succeeds.
        return match parse_int32(arg) {
            Some(value) if (0..=MESSAGE_BUFFER_MAX).contains(&value) => {
                factory.message_buffer_size = i64::from(value);
                factory
                    .message_buffer
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .set_max_bytes(value as usize);
                FactoryDirectiveOutcome::Ok
            }
            Some(_) => FactoryDirectiveOutcome::Invalid(format!(
                "MessageBufferSize must be between 0 and {MESSAGE_BUFFER_MAX}."
            )),
            None => FactoryDirectiveOutcome::Invalid(String::new()),
        };
    }
    if directive.eq_ignore_ascii_case("TrackOriginalContentLength") {
        warn_unless_global!();
        return match parse_bool(arg) {
            Some(value) => {
                factory.track_original_content_length = value;
                FactoryDirectiveOutcome::Ok
            }
            None => FactoryDirectiveOutcome::Invalid(String::new()),
        };
    }
    FactoryDirectiveOutcome::NotFactory
}

/// Applies a two-arg system factory directive
/// (`SystemRewriteDriverFactory::ParseAndSetOption2`: only
/// `CreateSharedMemoryMetadataCache`). Shared memory is
/// unsupported, so configured caches fail closed after the
/// verbatim size parse.
pub fn apply_factory_directive_2(
    factory: &mut DriverFactory,
    directive: &str,
    arg1: &str,
    arg2: &str,
    scope: Scope,
) -> FactoryDirectiveOutcome {
    if !directive.eq_ignore_ascii_case("CreateSharedMemoryMetadataCache") {
        return FactoryDirectiveOutcome::NotFactory;
    }
    let _ = (factory, arg1);
    if scope < Scope::LegacyProcess {
        return FactoryDirectiveOutcome::WarnIgnored(format!(
            "'{directive}' is global and is ignored at this scope"
        ));
    }
    match parse_int64(arg2) {
        Some(kb) if kb >= 0 => FactoryDirectiveOutcome::Invalid(
            "shared-memory metadata cache needs server integration".to_string(),
        ),
        _ => FactoryDirectiveOutcome::Invalid(
            "size_kb must be a positive 64-bit integer".to_string(),
        ),
    }
}

/// Post-configuration validation per server context
/// (`SystemRewriteDriverFactory::PostConfig` shape): signatures
/// need no separate pass (the port computes them purely), so this
/// pins the file-cache gate plus the statistics decisions.
pub struct PostConfigContext<'a> {
    /// Global options for the context.
    pub options: &'a NgxOptions,
}

/// Post-configuration outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostConfigOutcome {
    /// Failing context index plus message, if any.
    pub error: Option<(usize, String)>,
    /// Whether any context wants statistics (global registry).
    pub wants_global_statistics: bool,
    /// Contexts wanting per-vhost statistics.
    pub per_vhost_statistics: Vec<usize>,
}

/// Validates server contexts after configuration.
#[must_use]
pub fn post_config(factory: &DriverFactory, contexts: &[PostConfigContext]) -> PostConfigOutcome {
    let mut outcome = PostConfigOutcome {
        error: None,
        wants_global_statistics: false,
        per_vhost_statistics: Vec::new(),
    };
    for (index, context) in contexts.iter().enumerate() {
        if context.options.unplugged() {
            continue;
        }
        if context.options.core.enabled_state() == ps_config::options::EnabledState::On
            && context.options.system.file_cache_path.is_empty()
        {
            outcome.error = Some((index, "FileCachePath must not be empty".to_string()));
            return outcome;
        }
        if context.options.system.statistics_enabled {
            outcome.wants_global_statistics = true;
            if factory.use_per_vhost_statistics {
                outcome.per_vhost_statistics.push(index);
            }
        }
    }
    outcome
}

/// Remote-configuration fetcher warning, one per affected server
/// (`ps_init_module` remote block): with the native fetcher, a
/// remote refresh can only progress on the event loop, so the
/// request path either stalls (external cache) or skips the wait
/// (blocking cache, served from local configuration).
#[must_use]
pub fn remote_config_warning(
    use_native_fetcher: bool,
    remote_url: &str,
    has_external_cache: bool,
) -> Option<String> {
    if !use_native_fetcher || remote_url.is_empty() {
        return None;
    }
    if has_external_cache {
        Some(format!(
            "RemoteConfigurationUrl {remote_url} is set and UseNativeFetcher is on: \
             requests may stall up to RemoteConfigurationTimeoutMs while \
             the remote configuration refreshes."
        ))
    } else {
        Some(format!(
            "RemoteConfigurationUrl {remote_url} is set and UseNativeFetcher is on: \
             pagespeed will not wait for a remote configuration refresh on \
             the request path; requests during a refresh are served with \
             the local configuration."
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps_config::filter::RewriteLevel;
    use ps_config::options::{EnabledState, RewriteOptions};

    #[test]
    fn constructor_defaults() {
        let factory = DriverFactory::new("", -1);
        assert_eq!("", factory.hostname);
        assert_eq!(-1, factory.port);
        assert_eq!("/pagespeed_static/", factory.static_asset_prefix);
        assert_eq!(UNSET_MSEC, factory.resolver_timeout_ms);
        assert!(factory.use_per_vhost_statistics);
        assert!(!factory.install_crash_handler);
        assert_eq!(0, factory.message_buffer_size);
        assert!(!factory.track_original_content_length);
        assert!(!factory.list_outstanding_urls_on_error);
        assert!(!factory.force_caching);
        assert_eq!(-1, factory.num_rewrite_threads);
        assert_eq!(-1, factory.num_expensive_rewrite_threads);
        assert!(!factory.thread_counts_finalized);
        assert!(!factory.resolver_present);
        assert!(!factory.threads_started);
        assert!(!factory.shut_down);
        // Factory default options carry the beacon default.
        assert_eq!(
            "/ngx_pagespeed_beacon",
            factory.default_options.core.beacon_url().unwrap().http
        );
        assert_eq!(
            crate::message::MODULE_VERSION,
            factory.default_options.core.x_header_value()
        );
        assert_eq!(
            crate::message::MODULE_VERSION,
            factory.new_options().core.x_header_value()
        );
        assert!(factory
            .default_options
            .core
            .avoid_renaming_introspective_javascript());
        assert_eq!(
            DEFAULT_FILE_CACHE_CLEAN_INODE_LIMIT,
            factory.default_options.system.file_cache_clean_inode_limit
        );
        // Fresh driver options state CoreFilters explicitly.
        let options = factory.new_options();
        assert_eq!(RewriteLevel::CoreFilters, options.core.filters().level());
        assert!(options.core.was_set("l"));
    }

    #[test]
    fn set_main_conf_merges_process_scope_only() {
        let mut factory = DriverFactory::new("", -1);
        let mut main = RewriteOptions::new();
        main.set_option("ImageMaxRewritesAtOnce", "4", Scope::ProcessStrict)
            .unwrap();
        main.set_option("RewriteLevel", "CoreFilters", Scope::ProcessStrict)
            .unwrap();
        let mut main_ngx = NgxOptions::new();
        main_ngx.core = main;
        factory.set_main_conf(Some(&main_ngx));
        // Only process-scoped values transfer
        // (`ImageMaxRewritesAtOnce` is legacy-process-scoped;
        // `RewriteLevel` is not).
        assert!(factory.default_options.core.was_set("im"));
        assert!(!factory.default_options.core.was_set("l"));
        factory.set_main_conf(None);
    }

    #[test]
    fn resolver_lifecycle() {
        let mut factory = DriverFactory::new("", -1);
        // Serf never needs a resolver; the native fetcher does.
        assert!(factory.check_resolver());
        factory.factory_options.use_native_fetcher = true;
        assert!(!factory.check_resolver());
        factory.set_resolver(true);
        assert!(factory.check_resolver());
        // An unset wait defaults; an explicit one stores as-is.
        factory.set_resolver_timeout(UNSET_MSEC);
        assert_eq!(1000, factory.resolver_timeout_ms);
        factory.set_resolver_timeout(2500);
        assert_eq!(2500, factory.resolver_timeout_ms);
    }

    #[test]
    fn thread_counts_finalize_once() {
        let mut factory = DriverFactory::new("", -1);
        assert!(!factory.thread_counts_finalized);
        let info = factory.init().unwrap();
        assert_eq!(
            "No threading detected. Own threads: 1 Rewrite, 1 Expensive Rewrite.",
            info
        );
        assert_eq!(1, factory.num_rewrite_threads);
        assert_eq!(1, factory.num_expensive_rewrite_threads);
        assert!(factory.thread_counts_finalized);
        assert_eq!(None, factory.init());
        // Explicit positive counts survive; zero redetects.
        let mut factory = DriverFactory::new("", -1);
        factory.num_rewrite_threads = 8;
        factory.num_expensive_rewrite_threads = 0;
        let info = factory.init().unwrap();
        assert!(info.contains("8 Rewrite, 1 Expensive Rewrite"));
    }

    #[test]
    fn crash_handler_gate() {
        let mut factory = DriverFactory::new("", -1);
        assert!(!factory.should_install_crash_handler(true));
        assert!(!factory.should_install_crash_handler(false));
        factory.install_crash_handler = true;
        assert!(factory.should_install_crash_handler(true));
        assert!(!factory.should_install_crash_handler(false));
        // Both handlers start without a log.
        assert!(factory.message_handler.log().is_null());
        assert!(factory.html_parse_message_handler.log().is_null());
    }

    #[test]
    fn native_names_fall_through_to_ps_config() {
        // The nginx-native three never reach the system layer:
        // ps-config handles them before the fallback.
        use FactoryDirectiveOutcome::NotFactory;
        let mut factory = DriverFactory::new("", -1);
        assert_eq!(
            NotFactory,
            apply_factory_directive_1(&mut factory, "UseNativeFetcher", "on", Scope::ProcessStrict)
        );
        assert_eq!(
            NotFactory,
            apply_factory_directive_1(
                &mut factory,
                "NativeFetcherMaxKeepaliveRequests",
                "50",
                Scope::ProcessStrict
            )
        );
        assert_eq!(
            NotFactory,
            apply_factory_directive_1(
                &mut factory,
                "ProcessScriptVariables",
                "on",
                Scope::ProcessStrict
            )
        );
        assert_eq!(
            NotFactory,
            apply_factory_directive_1(&mut factory, "RewriteLevel", "x", Scope::ProcessStrict)
        );
    }

    #[test]
    fn system_factory_directives() {
        use FactoryDirectiveOutcome::{Invalid, NotFactory, Ok, WarnIgnored};
        let mut factory = DriverFactory::new("", -1);
        assert_eq!(
            Ok,
            apply_factory_directive_1(
                &mut factory,
                "StaticAssetPrefix",
                "/s/",
                Scope::ProcessStrict
            )
        );
        assert_eq!("/s/", factory.static_asset_prefix);
        // As-typed echo in the global-scope detail.
        assert_eq!(
            Invalid("'StaticAssetPrefix' is global and can't be set at this scope.".to_string()),
            apply_factory_directive_1(&mut factory, "StaticAssetPrefix", "/s/", Scope::Server)
        );
        assert_eq!(
            Invalid("'staticassetprefix' is global and can't be set at this scope.".to_string()),
            apply_factory_directive_1(&mut factory, "staticassetprefix", "/s/", Scope::Server)
        );
        assert_eq!(
            Ok,
            apply_factory_directive_1(&mut factory, "NumRewriteThreads", "4", Scope::ProcessStrict)
        );
        assert_eq!(4, factory.num_rewrite_threads);
        // Failed parses leave the detail empty and the field alone.
        assert_eq!(
            Invalid(String::new()),
            apply_factory_directive_1(
                &mut factory,
                "NumRewriteThreads",
                "many",
                Scope::ProcessStrict
            )
        );
        assert_eq!(4, factory.num_rewrite_threads);
        assert_eq!(
            Ok,
            apply_factory_directive_1(
                &mut factory,
                "UsePerVHostStatistics",
                "off",
                Scope::ProcessStrict
            )
        );
        assert!(!factory.use_per_vhost_statistics);
        // Warn set logs and ignores below top level.
        assert_eq!(
            Ok,
            apply_factory_directive_1(&mut factory, "ForceCaching", "on", Scope::ProcessStrict)
        );
        assert!(factory.force_caching);
        assert_eq!(
            WarnIgnored("'ForceCaching' is global and is ignored at this scope".to_string()),
            apply_factory_directive_1(&mut factory, "ForceCaching", "on", Scope::Server)
        );
        assert_eq!(
            WarnIgnored(
                "'ListOutstandingUrlsOnError' is global and is ignored at this scope".to_string()
            ),
            apply_factory_directive_1(
                &mut factory,
                "ListOutstandingUrlsOnError",
                "on",
                Scope::Directory
            )
        );
        assert_eq!(
            Ok,
            apply_factory_directive_1(
                &mut factory,
                "MessageBufferSize",
                "1024",
                Scope::ProcessStrict
            )
        );
        assert_eq!(1024, factory.message_buffer_size);
        assert_eq!(
            Invalid("MessageBufferSize must be between 0 and 1073741824.".to_string()),
            apply_factory_directive_1(
                &mut factory,
                "MessageBufferSize",
                "-1",
                Scope::ProcessStrict
            )
        );
        assert_eq!(
            Invalid(String::new()),
            apply_factory_directive_1(
                &mut factory,
                "MessageBufferSize",
                "lots",
                Scope::ProcessStrict
            )
        );
        assert_eq!(
            NotFactory,
            apply_factory_directive_1(&mut factory, "RewriteLevel", "x", Scope::ProcessStrict)
        );
        // Shared-memory metadata caches fail closed after parsing.
        assert_eq!(
            Invalid("shared-memory metadata cache needs server integration".to_string()),
            apply_factory_directive_2(
                &mut factory,
                "CreateSharedMemoryMetadataCache",
                "name",
                "1024",
                Scope::ProcessStrict
            )
        );
        assert_eq!(
            Invalid("size_kb must be a positive 64-bit integer".to_string()),
            apply_factory_directive_2(
                &mut factory,
                "CreateSharedMemoryMetadataCache",
                "name",
                "lots",
                Scope::ProcessStrict
            )
        );
        assert_eq!(
            Invalid("size_kb must be a positive 64-bit integer".to_string()),
            apply_factory_directive_2(
                &mut factory,
                "CreateSharedMemoryMetadataCache",
                "name",
                "-1",
                Scope::ProcessStrict
            )
        );
        assert_eq!(
            WarnIgnored(
                "'CreateSharedMemoryMetadataCache' is global and is ignored at this scope"
                    .to_string()
            ),
            apply_factory_directive_2(
                &mut factory,
                "CreateSharedMemoryMetadataCache",
                "name",
                "1024",
                Scope::Server
            )
        );
    }

    #[test]
    fn post_config_matrix() {
        let factory = DriverFactory::new("", -1);
        // Unplugged contexts skip everything.
        let mut unplugged = NgxOptions::new();
        unplugged.core.set_enabled_state(EnabledState::Unplugged);
        let outcome = post_config(
            &factory,
            &[PostConfigContext {
                options: &unplugged,
            }],
        );
        assert_eq!(None, outcome.error);
        assert!(!outcome.wants_global_statistics);
        // Enabled without a cache path fails at its index.
        let plain = NgxOptions::new();
        let outcome = post_config(&factory, &[PostConfigContext { options: &plain }]);
        assert_eq!(
            Some((0, "FileCachePath must not be empty".to_string())),
            outcome.error
        );
        // Statistics-enabled contexts request registries.
        let mut stats = NgxOptions::new();
        stats.system.statistics_enabled = true;
        stats.system.file_cache_path = "/cache".to_string();
        let outcome = post_config(&factory, &[PostConfigContext { options: &stats }]);
        assert_eq!(None, outcome.error);
        assert!(outcome.wants_global_statistics);
        assert_eq!(vec![0], outcome.per_vhost_statistics);
    }

    #[test]
    fn remote_warnings() {
        assert_eq!(None, remote_config_warning(false, "http://x", false));
        assert_eq!(None, remote_config_warning(true, "", false));
        let external = remote_config_warning(true, "http://x/rc", true).unwrap();
        assert!(external.contains("RemoteConfigurationUrl http://x/rc is set"));
        assert!(external.contains("may stall up to RemoteConfigurationTimeoutMs"));
        let blocking = remote_config_warning(true, "http://x/rc", false).unwrap();
        assert!(blocking.contains("will not wait for a remote configuration refresh"));
    }

    #[test]
    fn shutdown_idempotent() {
        let mut factory = DriverFactory::new("", -1);
        assert!(factory.shutdown().is_empty());
        assert!(factory.shut_down);
        assert!(factory.shutdown().is_empty());
        assert!(factory.shut_down);
    }

    #[test]
    fn root_init_registers_paths() {
        let mut factory = DriverFactory::new("", -1);
        let mut options = NgxOptions::new();
        options.system.file_cache_path = "/c".to_string();
        let messages = factory.root_init(&[&options]);
        assert_eq!(
            messages,
            vec!["Initializing shared memory for path: /c.".to_string()]
        );
        assert!(factory.caches.path("/c flush ").is_some());
        assert_eq!(
            factory.child_init_caches(),
            vec!["Reusing shared memory for path: /c.".to_string()]
        );
    }

    #[test]
    fn fetcher_map_caches_and_warns_once() {
        use crate::native_fetch::{FetchSubmit, StartDecision, SERF_FALLBACK_WARNING};
        let mut factory = DriverFactory::new("", -1);
        let options = NgxOptions::new();
        // Serf-configured (native off): warns once, on creation.
        let (key, warning) = factory.get_fetcher(&options);
        assert_eq!(Some(SERF_FALLBACK_WARNING.to_string()), warning);
        let (key2, warning) = factory.get_fetcher(&options);
        assert_eq!(key, key2);
        assert_eq!(None, warning);
        assert!(factory.fetcher(&key).is_some());
        assert!(factory.fetcher("bogus").is_none());
        // Distinct configs map distinctly.
        let mut other = NgxOptions::new();
        other.system.fetcher_proxy = "http://proxy/".to_string();
        let (other_key, _) = factory.get_fetcher(&other);
        assert_ne!(key, other_key);
        // Shutdown reports pending + active ids per fetcher.
        let fetcher = factory.fetcher_mut(&key).expect("mapped");
        assert_eq!(FetchSubmit::Queued(1), fetcher.fetch("http://h/a"));
        let drained = fetcher.drain_pending();
        assert_eq!(
            StartDecision::Proceed,
            fetcher.start_fetch(drained[0].clone())
        );
        assert_eq!(FetchSubmit::Queued(2), fetcher.fetch("http://h/b"));
        let mut failed = factory.shutdown();
        failed.sort();
        assert_eq!(failed, vec![(key.clone(), 1), (key.clone(), 2)]);
        assert!(factory.fetcher(&key).expect("kept").is_shutdown());
    }

    #[test]
    fn scheduler_thread_starts_once_and_joins_on_drop() {
        let mut factory = DriverFactory::new("", -1);
        assert!(!factory.threads_started);
        // The shared scheduler pumps alarms while started.
        let scheduler = factory.scheduler();
        let fired = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let probe = std::sync::Arc::clone(&fired);
        factory.start_threads();
        assert!(factory.threads_started);
        factory.start_threads();
        scheduler.add_alarm_at_us(
            scheduler.now_us() + 10_000,
            crate::scheduler::AlarmCallback::run_only(move || {
                probe.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while fired.load(std::sync::atomic::Ordering::SeqCst) == 0
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(1, fired.load(std::sync::atomic::Ordering::SeqCst));
        // Dropping joins promptly (a missed wakeup would stall here).
        let started = std::time::Instant::now();
        drop(factory);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "drop stalled: {:?}",
            started.elapsed()
        );
    }
}
