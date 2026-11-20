//! Mock nginx ABI harness (Slice 1 test seam).
//!
//! The real hooks mutate nginx globals and link nginx symbols,
//! so they cannot run in unit tests. This harness models the
//! pieces the port's wiring depends on — filter-top chaining
//! and phase-handler pushes — as plain data, so the composition
//! rules carry unit proofs here while Slice 7 proves them live
//! inside nginx.

#[cfg(test)]
use crate::filters::{FilterSlot, EXECUTION_ORDER, INIT_ORDER};

/// Fake filter-top slot: the id installed on top plus the id it
/// captured as its next pointer, per init step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FakeGrab {
    /// Init step name.
    pub name: &'static str,
    /// Header top captured (filter that was on top).
    pub header_next: usize,
    /// Body top captured.
    pub body_next: usize,
}

/// Fake installed filter ids (0/100 = the pre-existing tops).
pub const REST_HEADER: usize = 0;
pub const REST_BODY: usize = 100;

/// Link stubs for nginx symbols (`#[cfg(test)]` only).
///
/// Unit tests link without nginx, but test-reachable codegen
/// (module statics reference every hook; hooks reference nginx
/// functions and globals) needs every referenced symbol to
/// exist. These definitions satisfy the linker; tests never
/// execute them (all test paths take branches that avoid FFI —
/// null logs, fabricated structs, pure planners), so every body
/// is `unreachable!`. Production builds never compile this
/// module, so the real nginx symbols are untouched.
///
/// Link rule for new slices: keep FFI-touching functions out of
/// test-called paths, or extend this stub set in kind.
#[cfg(test)]
pub mod abi_stubs {
    use ngx::ffi::{
        ngx_array_t, ngx_chain_t, ngx_command_t, ngx_conf_t, ngx_connection_t, ngx_event_t,
        ngx_file_t, ngx_http_cleanup_t, ngx_http_client_body_handler_pt, ngx_http_request_t,
        ngx_int_t, ngx_list_t, ngx_log_t, ngx_module_t, ngx_pool_cleanup_t, ngx_pool_t,
        ngx_socket_t, ngx_str_t, ngx_uint_t, off_t, u_char,
    };
    use std::ffi::{c_char, c_int, c_void};

    macro_rules! stub_fn {
        ($name:ident ( $( $arg:ident : $ty:ty ),* $(,)? )) => {
            #[no_mangle]
            #[allow(non_snake_case)]
            /// Test-only linker stub; never executed.
            ///
            /// # Safety
            ///
            /// Tests never call this function.
            pub unsafe extern "C" fn $name($( $arg : $ty ),*) {
                let _ = ($( $arg, )*);
                unreachable!("mock nginx symbol executed in test");
            }
        };
        ($name:ident ( $( $arg:ident : $ty:ty ),* $(,)? ) -> $ret:ty) => {
            #[no_mangle]
            #[allow(non_snake_case)]
            /// Test-only linker stub; never executed.
            ///
            /// # Safety
            ///
            /// Tests never call this function.
            pub unsafe extern "C" fn $name($( $arg : $ty ),*) -> $ret {
                let _ = ($( $arg, )*);
                unreachable!("mock nginx symbol executed in test");
            }
        };
    }

    // Variadic in C; fixed-arity here (linker matches names only,
    // and tests never call).
    stub_fn!(
        ngx_log_error_core(
            level: ngx_uint_t,
            log: *mut ngx_log_t,
            err: c_int,
            fmt: *const c_char,
            len: usize,
            ptr: *const u8,
        )
    );
    stub_fn!(
        ngx_conf_log_error(
            level: ngx_uint_t,
            cf: *mut ngx_conf_t,
            err: c_int,
            fmt: *const c_char,
            len: usize,
            ptr: *const u8,
        )
    );
    stub_fn!(ngx_palloc(pool: *mut ngx_pool_t, size: usize) -> *mut c_void);
    stub_fn!(ngx_array_push(a: *mut ngx_array_t) -> *mut c_void);
    stub_fn!(ngx_pool_cleanup_add(p: *mut ngx_pool_t, size: usize) -> *mut ngx_pool_cleanup_t);
    stub_fn!(ngx_http_get_variable_index(cf: *mut ngx_conf_t, name: *mut ngx_str_t) -> ngx_int_t);
    stub_fn!(
        ngx_http_add_variable(
            cf: *mut ngx_conf_t,
            name: *mut ngx_str_t,
            flags: ngx_uint_t,
        ) -> *mut c_void
    );
    stub_fn!(ngx_http_finalize_request(r: *mut ngx_http_request_t, rc: ngx_int_t));
    stub_fn!(ngx_http_request_empty_handler(r: *mut ngx_http_request_t));
    stub_fn!(ngx_http_test_reading(r: *mut ngx_http_request_t));
    stub_fn!(ngx_http_block_reading(r: *mut ngx_http_request_t));
    stub_fn!(ngx_http_run_posted_requests(c: *mut ngx_connection_t));
    stub_fn!(ngx_http_core_run_phases(r: *mut ngx_http_request_t));
    stub_fn!(ngx_list_push(l: *mut ngx_list_t) -> *mut c_void);
    stub_fn!(
        ngx_connection_local_sockaddr(
            c: *mut ngx_connection_t,
            s: *mut ngx_str_t,
            port: ngx_uint_t,
        ) -> ngx_int_t
    );
    stub_fn!(ngx_get_connection(s: ngx_socket_t, log: *mut ngx_log_t) -> *mut ngx_connection_t);
    stub_fn!(ngx_free_connection(c: *mut ngx_connection_t));
    stub_fn!(ngx_close_connection(c: *mut ngx_connection_t));
    stub_fn!(ngx_handle_read_event(rev: *mut ngx_event_t, flags: ngx_uint_t) -> ngx_int_t);
    stub_fn!(ngx_pcalloc(pool: *mut ngx_pool_t, size: usize) -> *mut c_void);
    stub_fn!(ngx_alloc_chain_link(pool: *mut ngx_pool_t) -> *mut ngx_chain_t);
    stub_fn!(ngx_http_send_header(r: *mut ngx_http_request_t) -> ngx_int_t);
    stub_fn!(
        ngx_http_output_filter(r: *mut ngx_http_request_t, chain: *mut ngx_chain_t) -> ngx_int_t
    );
    stub_fn!(ngx_http_clean_header(r: *mut ngx_http_request_t));
    stub_fn!(ngx_http_discard_request_body(r: *mut ngx_http_request_t) -> ngx_int_t);
    stub_fn!(
        ngx_http_filter_finalize_request(
            r: *mut ngx_http_request_t,
            m: *mut ngx_module_t,
            error: ngx_int_t,
        ) -> ngx_int_t
    );
    stub_fn!(
        ngx_http_read_client_request_body(
            r: *mut ngx_http_request_t,
            post_handler: ngx_http_client_body_handler_pt,
        ) -> ngx_int_t
    );
    stub_fn!(
        ngx_read_file(file: *mut ngx_file_t, buf: *mut u_char, size: usize, offset: off_t) -> isize
    );
    stub_fn!(
        ngx_http_cleanup_add(r: *mut ngx_http_request_t, size: usize) -> *mut ngx_http_cleanup_t
    );
    stub_fn!(ngx_array_destroy(a: *mut ngx_array_t));
    stub_fn!(ngx_pnalloc(pool: *mut ngx_pool_t, size: usize) -> *mut c_void);
    stub_fn!(
        ngx_conf_set_flag_slot(
            cf: *mut ngx_conf_t,
            cmd: *mut ngx_command_t,
            conf: *mut c_void,
        ) -> *mut c_char
    );
    stub_fn!(
        ngx_conf_set_enum_slot(
            cf: *mut ngx_conf_t,
            cmd: *mut ngx_command_t,
            conf: *mut c_void,
        ) -> *mut c_char
    );
    stub_fn!(
        ngx_conf_set_bitmask_slot(
            cf: *mut ngx_conf_t,
            cmd: *mut ngx_command_t,
            conf: *mut c_void,
        ) -> *mut c_char
    );
    stub_fn!(
        ngx_http_types_slot(
            cf: *mut ngx_conf_t,
            cmd: *mut ngx_command_t,
            conf: *mut c_void,
        ) -> *mut c_char
    );

    // Referenced globals. Kept uninhabited: tests read only our
    // own statics, never these.
    #[no_mangle]
    #[allow(non_upper_case_globals)]
    pub static mut ngx_http_top_header_filter: usize = 0;
    #[no_mangle]
    #[allow(non_upper_case_globals)]
    pub static mut ngx_http_top_body_filter: usize = 0;
    #[no_mangle]
    #[allow(non_upper_case_globals)]
    pub static mut ngx_http_core_module: usize = 0;
    #[no_mangle]
    #[allow(non_upper_case_globals)]
    pub static mut ngx_core_module: usize = 0;
    #[no_mangle]
    #[allow(non_upper_case_globals)]
    pub static mut ngx_cycle: usize = 0;
    #[no_mangle]
    #[allow(non_upper_case_globals)]
    pub static mut ngx_terminate: c_int = 0;
    #[no_mangle]
    #[allow(non_upper_case_globals)]
    pub static mut ngx_exiting: ngx_uint_t = 0;
    #[no_mangle]
    #[allow(non_upper_case_globals)]
    pub static mut ngx_http_module: usize = 0;
    #[no_mangle]
    #[allow(non_upper_case_globals)]
    pub static mut ngx_process: usize = 0;
    #[no_mangle]
    #[allow(non_upper_case_globals)]
    pub static mut ngx_test_config: usize = 0;
    #[no_mangle]
    #[allow(non_upper_case_globals)]
    pub static mut ngx_event_actions: usize = 0;
    #[no_mangle]
    #[allow(non_upper_case_globals)]
    pub static mut ngx_event_flags: usize = 0;
}

/// Simulates the four inits over fake tops, returning each
/// step's captured next-pointers in init order.
#[must_use]
pub fn simulate_chain() -> Vec<FakeGrab> {
    // Tops walk down as each init grabs them; ids mirror the
    // slots each namespace installs.
    let mut header_top = REST_HEADER;
    let mut body_top = REST_BODY;
    let mut grabs = Vec::new();
    // in_place grabs both.
    grabs.push(FakeGrab {
        name: "in_place",
        header_next: header_top,
        body_next: body_top,
    });
    header_top = 10;
    body_top = 11;
    // fix_headers grabs the header top only.
    grabs.push(FakeGrab {
        name: "fix_headers",
        header_next: header_top,
        body_next: body_top,
    });
    header_top = 20;
    // base_fetch grabs the body top only (its header save is dead).
    grabs.push(FakeGrab {
        name: "base_fetch",
        header_next: header_top,
        body_next: body_top,
    });
    body_top = 30;
    // html_rewrite grabs both.
    grabs.push(FakeGrab {
        name: "html_rewrite",
        header_next: header_top,
        body_next: body_top,
    });
    grabs
}

/// Walks a fake chain from a top id through captured nexts.
#[cfg(test)]
fn walk_chain(grabs: &[FakeGrab], header: bool, top: usize, rest: usize) -> Vec<usize> {
    // Rebuild next-map: installed id -> captured top.
    let mut next = std::collections::HashMap::new();
    // Install ids in init order: in_place header=10 body=11,
    // fix=20, base=30, html header=40 body=41.
    let installed = if header {
        [
            (10usize, grabs[0].header_next),
            (20, grabs[1].header_next),
            (40, grabs[3].header_next),
        ]
    } else {
        [
            (11usize, grabs[0].body_next),
            (30, grabs[2].body_next),
            (41, grabs[3].body_next),
        ]
    };
    for (id, captured) in installed {
        next.insert(id, captured);
    }
    let mut order = Vec::new();
    let mut current = top;
    while current != rest {
        order.push(current);
        current = *next.get(&current).unwrap_or(&rest);
    }
    order
}

/// Simulates postconfiguration's phase pushes: the content-phase
/// handler plus the post-read self-fetch handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhasePush {
    /// Phase index.
    pub phase: u32,
    /// Handler name.
    pub handler: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_composes_to_execution_order() {
        let grabs = simulate_chain();
        assert_eq!(
            INIT_ORDER.to_vec(),
            grabs.iter().map(|g| g.name).collect::<Vec<_>>()
        );
        // Final tops: html_rewrite header=40, body=41.
        let header = walk_chain(&grabs, true, 40, REST_HEADER);
        let body = walk_chain(&grabs, false, 41, REST_BODY);
        assert_eq!(vec![40, 20, 10], header);
        assert_eq!(vec![41, 30, 11], body);
        // Header walk: html → fix → in_place; body walk:
        // html → base → in_place. The six execution slots are
        // exactly these two walks interleaved.
        assert_eq!(6, EXECUTION_ORDER.len());
        let header_slots: Vec<FilterSlot> = EXECUTION_ORDER
            .into_iter()
            .filter(|s| {
                matches!(
                    s,
                    FilterSlot::HtmlRewriteHeader
                        | FilterSlot::FixHeadersHeader
                        | FilterSlot::InPlaceHeader
                )
            })
            .collect();
        let body_slots: Vec<FilterSlot> = EXECUTION_ORDER
            .into_iter()
            .filter(|s| {
                matches!(
                    s,
                    FilterSlot::HtmlRewriteBody
                        | FilterSlot::BaseFetchBody
                        | FilterSlot::InPlaceBody
                )
            })
            .collect();
        assert_eq!(
            [
                FilterSlot::HtmlRewriteHeader,
                FilterSlot::FixHeadersHeader,
                FilterSlot::InPlaceHeader
            ],
            header_slots.as_slice()
        );
        assert_eq!(
            [
                FilterSlot::HtmlRewriteBody,
                FilterSlot::BaseFetchBody,
                FilterSlot::InPlaceBody
            ],
            body_slots.as_slice()
        );
        assert_eq!(3, header.len());
        assert_eq!(3, body.len());
    }
}
