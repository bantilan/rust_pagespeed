//! Configuration create/merge (`ps_create_*`, `ps_merge_*`).
//!
//! Ports the nginx configuration half of `ngx_pagespeed.cc`:
//! the main/server/location conf shapes, `NgxRewriteOptions`
//! (core options + system options + script lines), the
//! order-sensitive option merge, server-context creation
//! inputs, and the FileCachePath validation. Server-context
//! construction is Slice 2 and the driver factory is Slice 6;
//! both enter here as recorded inputs, never fabricated.

use ngx::ffi::ngx_int_t;
use ps_config::options::{NginxFactoryOptions, NginxSystemOptions, RewriteOptions};

/// One script-variable line (`ScriptLine`): raw directive words
/// plus the scope they were parsed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptLine {
    /// Directive words, e.g. `["RewriteLevel", "CoreFilters"]`.
    pub args: Vec<String>,
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
    // System options merge like scalars: the child's non-empty
    // paths win (C++ `Option<T>::Merge` keeps the set value).
    if !child_specific.system.file_cache_path.is_empty() {
        rebased.system.file_cache_path = child_specific.system.file_cache_path.clone();
    }
    if !child_specific.system.log_dir.is_empty() {
        rebased.system.log_dir = child_specific.system.log_dir.clone();
    }
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

/// Seeded server context: the recorded `MakeNgxServerContext`
/// inputs (`ps_merge_srv_conf`). Slice 2 builds the live
/// context around this seed.
#[derive(Debug, Clone)]
pub struct SeededContext {
    /// Dummy hostname (always `DUMMY_HOSTNAME`).
    pub hostname: String,
    /// Dummy port (negated context count).
    pub dummy_port: i32,
    /// Merged global options.
    pub options: NgxOptions,
}

/// Main conf (`ps_main_conf_t`).
#[derive(Debug, Default)]
pub struct MainConf {
    /// Factory slot: live generation plus the deleted-once latch
    /// (`driver_factory`, `factory_deleted`).
    pub factory: FactorySlot,
    /// Server-context counter (`server_context_count`).
    pub server_context_count: u32,
    /// Factory-owned directive state.
    pub factory_options: NginxFactoryOptions,
}

/// Server conf (`ps_srv_conf_t`, Slice 1 fields).
#[derive(Debug, Default)]
pub struct SrvConf {
    /// Level options (moved into the context at merge).
    pub options: Option<NgxOptions>,
    /// Seeded context after merge (`server_context` inputs).
    pub context: Option<SeededContext>,
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
/// Counts the server, rebases options, seeds the context with
/// the dummy identity, and validates the cache path. `is_dir`
/// answers the directory check (live filesystem at runtime).
/// `http2_index` records the `http2` variable lookup (done
/// conditionally in C++; the index travels with the context in
/// Slice 2 — taken here to pin the call order).
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
    // The context applies its own defaults before the file
    // options merge in (Slice 2 owns the live context; the seed
    // records the merge inputs and order).
    let mut global = NgxOptions::new();
    global.core.merge(&options.core);
    if !options.system.file_cache_path.is_empty() {
        global.system.file_cache_path = options.system.file_cache_path.clone();
    }
    if !options.system.log_dir.is_empty() {
        global.system.log_dir = options.system.log_dir.clone();
    }
    options.copy_script_lines_to(&mut global);
    validate_file_cache_path(
        &global.system.file_cache_path,
        global.unplugged(),
        global.standby(),
        is_dir,
    )?;
    srv.context = Some(SeededContext {
        hostname: DUMMY_HOSTNAME.to_string(),
        dummy_port: dummy_port(main.server_context_count),
        options: global,
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
    use ps_config::options::apply_nginx_directive;
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
                });
            }
        }
        options
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
        // Without options: counted but untouched.
        let mut srv = SrvConf::default();
        merge_srv_conf(&mut main, None, &mut srv, 7, |_| true).unwrap();
        assert_eq!(2, main.server_context_count);
        assert!(srv.context.is_none());
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
    fn factory_delete_guards() {
        let mut slot = FactorySlot { generation: Some(3), deleted: false };
        let mut active = Some(3);
        delete_cycle_factory(&mut slot, &mut active);
        assert_eq!(None, slot.generation);
        assert!(slot.deleted);
        assert_eq!(None, active);
        // Second call is a no-op.
        delete_cycle_factory(&mut slot, &mut active);
        // A foreign active generation is never untracked.
        let mut slot = FactorySlot { generation: Some(4), deleted: false };
        let mut active = Some(9);
        delete_cycle_factory(&mut slot, &mut active);
        assert_eq!(Some(9), active);
        assert!(slot.deleted);
    }
}
