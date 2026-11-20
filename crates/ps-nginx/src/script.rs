//! Request-time script-variable execution (`ExecuteScriptVariables`).
//!
//! Ports the execution half: each recorded line's args resolve
//! through the injected substituter (Slice 6 runs the compiled
//! nginx programs against the live request), then the line applies
//! at its recorded scope. Any substitution or parse failure
//! disables optimization (`Off`, never silently partial) and
//! reports false. Recording plus compilation happen at config
//! time (Slice 6, needs the live `cf`).

use crate::conf::NgxOptions;
use ps_config::options::{apply_nginx_directive, EnabledState, NginxFactoryOptions};

/// Executes script lines: substitute each arg, apply the line at
/// its scope, fail closed to `Off` on any error. `substitute`
/// maps `(line_index, arg_index, raw)` to the request-time value,
/// or `None` when evaluation fails.
pub fn execute_script_lines(
    options: &mut NgxOptions,
    factory: &mut NginxFactoryOptions,
    substitute: &dyn Fn(usize, usize, &str) -> Option<String>,
) -> bool {
    for (line_index, line) in options.script_lines.iter().enumerate() {
        if line.args.is_empty() {
            options.core.set_enabled_state(EnabledState::Off);
            return false;
        }
        let mut resolved: Vec<String> = Vec::with_capacity(line.args.len());
        let mut failed = false;
        for (arg_index, arg) in line.args.iter().enumerate() {
            match substitute(line_index, arg_index, arg) {
                Some(value) => resolved.push(value),
                None => {
                    failed = true;
                    break;
                }
            }
        }
        if failed {
            options.core.set_enabled_state(EnabledState::Off);
            return false;
        }
        let refs: Vec<&str> = resolved.iter().map(String::as_str).collect();
        if apply_nginx_directive(
            &mut options.core,
            factory,
            &mut options.system,
            refs[0],
            &refs[1..],
            line.scope,
        )
        .is_err()
        {
            options.core.set_enabled_state(EnabledState::Off);
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conf::ScriptLine;
    use ps_config::filter::RewriteLevel;
    use ps_config::scope::Scope;

    fn line(args: &[&str]) -> ScriptLine {
        ScriptLine {
            args: args.iter().map(|s| s.to_string()).collect(),
            scope: Scope::Server,
        }
    }

    #[test]
    fn executes_and_applies() {
        let mut options = NgxOptions::new();
        options.script_lines.push(line(&["RewriteLevel", "CoreFilters"]));
        let mut factory = NginxFactoryOptions::default();
        assert!(execute_script_lines(&mut options, &mut factory, &|_, _, raw| {
            Some(raw.to_string())
        }));
        assert_eq!(RewriteLevel::CoreFilters, options.core.filters().level());
    }

    #[test]
    fn substitution_rewrites_args() {
        let mut options = NgxOptions::new();
        options.script_lines.push(line(&["RewriteLevel", "$level"]));
        let mut factory = NginxFactoryOptions::default();
        assert!(execute_script_lines(&mut options, &mut factory, &|_, index, raw| {
            Some(if index == 1 { "PassThrough".to_string() } else { raw.to_string() })
        }));
        assert_eq!(RewriteLevel::PassThrough, options.core.filters().level());
    }

    #[test]
    fn failures_disable() {
        // Substitution failure.
        let mut options = NgxOptions::new();
        options.script_lines.push(line(&["RewriteLevel", "$level"]));
        let mut factory = NginxFactoryOptions::default();
        assert!(!execute_script_lines(&mut options, &mut factory, &|_, _, _| None));
        assert_eq!(EnabledState::Off, options.core.enabled_state());
        // Parse failure (bogus value for a known option).
        let mut options = NgxOptions::new();
        options.script_lines.push(line(&["RewriteLevel", "BogusLevel"]));
        let mut factory = NginxFactoryOptions::default();
        assert!(!execute_script_lines(&mut options, &mut factory, &|_, _, raw| {
            Some(raw.to_string())
        }));
        assert_eq!(EnabledState::Off, options.core.enabled_state());
        // Unknown directives fail closed too.
        let mut options = NgxOptions::new();
        options.script_lines.push(line(&["BogusDirective", "x"]));
        let mut factory = NginxFactoryOptions::default();
        assert!(!execute_script_lines(&mut options, &mut factory, &|_, _, raw| {
            Some(raw.to_string())
        }));
        assert_eq!(EnabledState::Off, options.core.enabled_state());
    }
}
