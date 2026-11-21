//! Event pipe channel (`NgxEventConnection` core).
//!
//! Ports the pipe mechanics: the event struct, the
//! write/backoff/drop rules, single-event drain dispatch, and the
//! init failure texts. Nginx fd registration plus the read-event
//! handler stay FFI (Slice 7); fd I/O and backoff sleeps run here
//! over real pipes, so the rules carry unit proofs with
//! `libc::pipe` pairs.
//!
//! Wire note: C++ ships a connection back-pointer in each event;
//! both pipe ends are always this port, and the reader knows its
//! channel, so the event carries only the type byte and the
//! sender (a reclaimed `Arc::into_raw` holder on the live path).

use ps_cache::timer::SECOND_US;
use std::ffi::c_void;
use std::os::fd::RawFd;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Pipe event (`ps_event_data` without the back-pointer).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PipeEvent {
    /// Event type byte (`H`/`F`/`D`, `X` for single-type users).
    pub event_type: u8,
    /// Opaque sender (reclaimed by the reader).
    pub sender: *mut c_void,
}

impl PartialEq for PipeEvent {
    fn eq(&self, other: &Self) -> bool {
        self.event_type == other.event_type && std::ptr::eq(self.sender, other.sender)
    }
}

impl Eq for PipeEvent {}

/// Fallback event byte (`WriteEvent(sender)` overload).
pub const EVENT_ANY: u8 = b'X';

/// Write backoff policy (verbatim C++ defaults).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteBackoff {
    /// Give up after this many `EAGAIN` retries.
    pub max_retries: u32,
    /// First sleep between retries.
    pub initial_us: u64,
    /// Sleep cap (doubles toward it).
    pub max_us: u64,
}

/// Verbatim backoff (50 retries, 100us doubling to 100ms).
pub const DEFAULT_BACKOFF: WriteBackoff = WriteBackoff {
    max_retries: 50,
    initial_us: 100,
    max_us: 100_000,
};

/// Pipe write outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipeWrite {
    /// Event queued.
    Sent,
    /// Channel torn down: the event was dropped.
    Closed,
    /// Not sent with the channel alive: full past the retry
    /// budget, an I/O error, or a short write (C++ folds every
    /// failure into `false`).
    Full,
}

/// Drain outcome (`ReadAndNotify` result).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainOutcome {
    /// One event dispatched (reads strictly one per call: event
    /// handlers reenter, so batching could reorder).
    Dispatched(PipeEvent),
    /// Nothing available.
    Empty,
    /// EOF, error, or a short read (short reads are impossible by
    /// protocol — whole structs under `PIPE_BUF` — and C++ would
    /// dispatch the garbage; the port ends instead).
    Ended,
}

/// `pipe()` failure (`Init`, `EMERG`).
pub const PIPE_FAILED: &str = "pagespeed: pipe() failed";
/// Read-end nonblocking failure (`Init`, `EMERG`, Linux expansion
/// of the `ngx_nonblocking_n` prefix, double space verbatim).
pub const PIPE_READ_NONBLOCKING_FAILED: &str = "fcntl(O_NONBLOCK)pagespeed:  pipe[0] failed";
/// Write-end nonblocking failure (same shape).
pub const PIPE_WRITE_NONBLOCKING_FAILED: &str = "fcntl(O_NONBLOCK)pagespeed:  pipe[1] failed";
/// Nginx connection failure (`Init`, `EMERG`).
pub const CONNECTION_FAILED: &str = "pagespeed: failed to create connection.";

/// Created pipe ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipeFds {
    /// Read end (nginx-registered live; test-drained here).
    pub read_fd: RawFd,
    /// Write end (owned by the channel).
    pub write_fd: RawFd,
}

/// Creates a pipe (`Init` fd half). Returns `None` when `pipe()`
/// fails (the FFI logs `PIPE_FAILED`).
#[must_use]
pub fn open_pipe() -> Option<PipeFds> {
    let mut fds = [0; 2];
    // SAFETY: `pipe` fills both entries on success.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return None;
    }
    Some(PipeFds {
        read_fd: fds[0],
        write_fd: fds[1],
    })
}

/// Marks an end nonblocking (`ngx_nonblocking`).
pub fn make_nonblocking(fd: RawFd) -> bool {
    // SAFETY: `fcntl` flag dance on a live fd.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 {
            return false;
        }
        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) == 0
    }
}

/// Bumps the pipe capacity past 200 KiB when the platform allows
/// (`F_SETPIPE_SZ`, best-effort like C++).
pub fn bump_pipe_capacity(fd: RawFd) {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: best-effort capacity hint; the result is ignored.
        unsafe {
            libc::fcntl(fd, libc::F_SETPIPE_SZ, 200 * 1024);
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = fd;
    }
}

/// Closes one fd, ignoring errors.
pub fn close_fd(fd: RawFd) {
    // SAFETY: caller-owned fd; errors are irrelevant on close.
    unsafe {
        libc::close(fd);
    }
}

/// Write-side state (the read side is FFI-owned live).
#[derive(Debug)]
struct WriteState {
    write_fd: RawFd,
    closed: bool,
}

/// Pipe channel (`NgxEventConnection` without nginx registration).
#[derive(Debug)]
pub struct PipeChannel {
    read_fd: RawFd,
    state: Mutex<WriteState>,
}

impl PipeChannel {
    /// Wraps open pipe ends (read end registered live separately).
    #[must_use]
    pub fn new(fds: PipeFds) -> Self {
        PipeChannel {
            read_fd: fds.read_fd,
            state: Mutex::new(WriteState {
                write_fd: fds.write_fd,
                closed: false,
            }),
        }
    }

    /// Read end (drain/tests; closed through nginx live).
    #[must_use]
    pub fn read_fd(&self) -> RawFd {
        self.read_fd
    }

    /// Writes one event (`WriteEvent` with the default budget).
    pub fn write_event(&self, event_type: u8, sender: *mut c_void) -> PipeWrite {
        self.write_event_with_backoff(event_type, sender, DEFAULT_BACKOFF)
    }

    /// Writes one event with an explicit retry budget (tests use
    /// a tiny one; the full budget sleeps seconds when full).
    pub fn write_event_with_backoff(
        &self,
        event_type: u8,
        sender: *mut c_void,
        backoff: WriteBackoff,
    ) -> PipeWrite {
        // SAFETY: zero is valid for the byte and the pointer.
        let mut event: PipeEvent = unsafe { std::mem::zeroed() };
        event.event_type = event_type;
        event.sender = sender;
        let bytes: [u8; std::mem::size_of::<PipeEvent>()] =
            // SAFETY: plain byte copy of the zeroed struct.
            unsafe { std::mem::transmute_copy(&event) };
        let mut retries = 0;
        let mut backoff_us = backoff.initial_us;
        let mut state = lock(&self.state);
        if state.closed {
            return PipeWrite::Closed;
        }
        loop {
            // SAFETY: `write` of a live buffer to a live fd.
            let wrote = unsafe {
                libc::write(state.write_fd, bytes.as_ptr() as *const c_void, bytes.len())
            };
            if wrote as usize == bytes.len() {
                return PipeWrite::Sent;
            }
            if wrote >= 0 {
                // Short write (impossible under `PIPE_BUF`; C++
                // `CHECK`-crashes): fail like any other error.
                return PipeWrite::Full;
            }
            match std::io::Error::last_os_error().raw_os_error() {
                Some(code) if code == libc::EINTR => continue,
                Some(code) if code == libc::EAGAIN || code == libc::EWOULDBLOCK => {
                    retries += 1;
                    if retries > backoff.max_retries {
                        return PipeWrite::Full;
                    }
                    // Sleep without the lock so shutdown is never
                    // starved by a writer spinning on a full pipe.
                    drop(state);
                    std::thread::sleep(Duration::from_micros(backoff_us));
                    state = lock(&self.state);
                    if state.closed {
                        return PipeWrite::Closed;
                    }
                    // Doubles past the cap by one step, verbatim.
                    if backoff_us < backoff.max_us {
                        backoff_us = backoff_us.saturating_mul(2);
                    }
                }
                _ => return PipeWrite::Full,
            }
        }
    }

    /// Tears the write side down (`Shutdown` write half): latches
    /// closed and closes the fd under the lock, so a writer
    /// either finishes first or drops. The read side closes
    /// through nginx (FFI).
    pub fn shutdown_write(&self) {
        let mut state = lock(&self.state);
        if !state.closed {
            state.closed = true;
            close_fd(state.write_fd);
            state.write_fd = -1;
        }
    }

    /// Drains one event from the read end (`Drain`).
    pub fn drain(&self, on_event: &mut dyn FnMut(PipeEvent)) -> DrainOutcome {
        drain_fd(self.read_fd, on_event)
    }
}

/// Reads and dispatches at most one event (`ReadAndNotify`).
pub fn drain_fd(read_fd: RawFd, on_event: &mut dyn FnMut(PipeEvent)) -> DrainOutcome {
    let mut bytes = [0u8; std::mem::size_of::<PipeEvent>()];
    loop {
        // SAFETY: `read` into a live buffer from a live fd.
        let got = unsafe { libc::read(read_fd, bytes.as_mut_ptr() as *mut c_void, bytes.len()) };
        if got < 0 {
            match std::io::Error::last_os_error().raw_os_error() {
                Some(code) if code == libc::EINTR => continue,
                Some(code) if code == libc::EAGAIN || code == libc::EWOULDBLOCK => {
                    return DrainOutcome::Empty;
                }
                _ => return DrainOutcome::Ended,
            }
        }
        if got as usize != bytes.len() {
            return DrainOutcome::Ended;
        }
        // SAFETY: the buffer holds exactly one event image.
        let event: PipeEvent = unsafe { std::ptr::read(bytes.as_ptr() as *const PipeEvent) };
        on_event(event);
        return DrainOutcome::Dispatched(event);
    }
}

/// Locks a channel mutex (poison recovers: a panicked writer must
/// not wedge shutdown).
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Base-fetch event channel slot (the `event_connection` global).
static BASE_FETCH_CHANNEL: Mutex<Option<PipeChannel>> = Mutex::new(None);

/// Installs a channel into a slot (`Initialize` state half):
/// panics when one is already set (`CHECK(event_connection)`).
pub fn install_channel_into(slot: &Mutex<Option<PipeChannel>>, channel: PipeChannel) {
    let mut guard = lock(slot);
    assert!(guard.is_none(), "event connection already set");
    *guard = Some(channel);
}

/// Installs the base-fetch channel (worker init).
pub fn install_base_fetch_channel(channel: PipeChannel) {
    install_channel_into(&BASE_FETCH_CHANNEL, channel);
}

/// Detaches the channel from a slot (`Terminate` detach half).
pub fn take_channel_from(slot: &Mutex<Option<PipeChannel>>) -> Option<PipeChannel> {
    lock(slot).take()
}

/// Detaches the base-fetch channel (worker exit).
pub fn take_base_fetch_channel() -> Option<PipeChannel> {
    take_channel_from(&BASE_FETCH_CHANNEL)
}

/// Borrows the base-fetch slot for per-request sinks (the HTML
/// branch holds this for the life of its fetch).
#[must_use]
pub fn base_fetch_channel_slot() -> &'static Mutex<Option<PipeChannel>> {
    &BASE_FETCH_CHANNEL
}

/// Peeks the installed read end, if any (exit drain).
#[must_use]
pub fn base_fetch_read_fd() -> Option<RawFd> {
    lock(&BASE_FETCH_CHANNEL).as_ref().map(PipeChannel::read_fd)
}

/// Event channel: posts an event for a fetch, reporting delivery.
pub trait EventChannel {
    /// Posts an event; false when the channel is gone.
    fn write_event(&mut self, event: crate::fetch::FetchEvent) -> bool;
}

/// Posts one event through a channel slot (live `EventChannel`):
/// a missing channel drops the event like the C++ null.
pub struct PipePost<'a> {
    slot: &'a Mutex<Option<PipeChannel>>,
    sender: *mut c_void,
}

impl<'a> PipePost<'a> {
    /// Borrows a slot for one post.
    #[must_use]
    pub fn new(slot: &'a Mutex<Option<PipeChannel>>, sender: *mut c_void) -> Self {
        PipePost { slot, sender }
    }

    /// Posts through the base-fetch slot.
    #[must_use]
    pub fn base_fetch(sender: *mut c_void) -> PipePost<'static> {
        PipePost::new(&BASE_FETCH_CHANNEL, sender)
    }
}

impl EventChannel for PipePost<'_> {
    fn write_event(&mut self, event: crate::fetch::FetchEvent) -> bool {
        let guard = lock(self.slot);
        match guard.as_ref() {
            Some(channel) => matches!(
                channel.write_event(crate::fetch::event_byte(event), self.sender),
                PipeWrite::Sent
            ),
            None => false,
        }
    }
}

/// Arc-paired post (`RequestCollection` lifetime half): the first
/// write leaks one `Arc` into the pipe for the reader to reclaim;
/// a failed write reclaims it immediately. At most one write per
/// adapter (every rule posts at most once per call).
pub struct ArcPost<'a> {
    slot: &'a Mutex<Option<PipeChannel>>,
    arc: Option<Arc<crate::fetch::SharedBaseFetch>>,
}

impl<'a> ArcPost<'a> {
    /// Borrows a slot, pairing one `Arc` with the coming post.
    #[must_use]
    pub fn new(
        slot: &'a Mutex<Option<PipeChannel>>,
        fetch: &Arc<crate::fetch::SharedBaseFetch>,
    ) -> Self {
        ArcPost {
            slot,
            arc: Some(Arc::clone(fetch)),
        }
    }
}

impl EventChannel for ArcPost<'_> {
    fn write_event(&mut self, event: crate::fetch::FetchEvent) -> bool {
        let arc = self.arc.take().expect("one post per adapter");
        let raw = Arc::into_raw(arc) as *mut c_void;
        let mut post = PipePost::new(self.slot, raw);
        if post.write_event(event) {
            true
        } else {
            // SAFETY: leaked just above, never shared.
            drop(unsafe { Arc::from_raw(raw as *const crate::fetch::SharedBaseFetch) });
            false
        }
    }
}

/// Terminate drain budget (`30 * Timer::kSecondUs`).
pub const TERMINATE_TIMEOUT_US: i64 = 30 * SECOND_US;
/// Sleep between drain rounds (100us).
pub const TERMINATE_ROUNDUP_SLEEP_US: u64 = 100;

/// Roundup notice (`Terminate`, info).
#[must_use]
pub fn terminate_roundup_message(active: i32) -> String {
    format!("NgxBaseFetch::Terminate rounding up {active} active base fetches.")
}

/// Drain-timeout warning (`Terminate`, warning).
#[must_use]
pub fn terminate_timeout_message(active: i32) -> String {
    format!("NgxBaseFetch::Terminate timed out with {active} active base fetches.")
}

/// One drain-round decision (`Terminate` loop).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminateStep {
    /// Fetches remain and time is left: drain and sleep.
    KeepDraining,
    /// No fetches left: detach and shut down.
    Done,
    /// Time ran out with fetches left: warn, then detach anyway.
    TimedOut,
}

/// Decides one drain round from the live count and deadline.
#[must_use]
pub const fn terminate_step(active: i32, now_us: i64, end_us: i64) -> TerminateStep {
    if active <= 0 {
        TerminateStep::Done
    } else if end_us > now_us {
        TerminateStep::KeepDraining
    } else {
        TerminateStep::TimedOut
    }
}

// ---------------------------------------------------------------------------
// Nginx registration (FFI; live-tested in Slice 7)
// ---------------------------------------------------------------------------

/// Registers the read end with the event loop
/// (`CreateNgxConnection`): returns the channel connection for
/// `close_read_connection`, or null on failure.
///
/// # Safety
///
/// `cycle` must point at a live `ngx_cycle_t`; `read_fd` must be a
/// live pipe read end.
pub unsafe fn register_read_fd(
    cycle: *mut ngx::ffi::ngx_cycle_t,
    read_fd: RawFd,
) -> *mut ngx::ffi::ngx_connection_t {
    use ngx::ffi::{
        ngx_connection_t, ngx_event_actions, ngx_event_flags, ngx_free_connection,
        ngx_get_connection, ngx_int_t, ngx_uint_t, NGX_OK, NGX_USE_EPOLL_EVENT,
    };
    // `NGX_READ_EVENT` is a platform macro (epoll here); the
    // `ngx_add_*` names are macros over `ngx_event_actions`.
    const READ_EVENT: ngx_int_t = (libc::EPOLLIN | libc::EPOLLRDHUP) as ngx_int_t;
    // SAFETY: caller guarantees a live cycle; the fd is live.
    unsafe {
        let log = (*cycle).log;
        let c = ngx_get_connection(read_fd, log);
        if c.is_null() {
            return std::ptr::null_mut();
        }
        (*c).pool = (*cycle).pool;
        let rev = (*c).read;
        let wev = (*c).write;
        (*rev).log = log;
        (*wev).log = log;
        (*rev).set_channel(1);
        (*wev).set_channel(1);
        (*rev).handler = Some(base_fetch_read_event_handler);
        // Snapshot the event-module hooks (raw reads: the module
        // table is fixed after nginx startup).
        let add_conn = std::ptr::addr_of!(ngx_event_actions.add_conn).read();
        let add = std::ptr::addr_of!(ngx_event_actions.add).read();
        let event_flags = std::ptr::addr_of!(ngx_event_flags).read();
        let rc = match add_conn {
            Some(add_conn) if (event_flags & NGX_USE_EPOLL_EVENT as ngx_uint_t) == 0 => add_conn(c),
            _ => add.expect("event module without add")(rev, READ_EVENT, 0),
        };
        if rc != NGX_OK as ngx_int_t {
            ngx_free_connection(c);
            return std::ptr::null_mut();
        }
        c as *mut ngx_connection_t
    }
}

/// Closes a registered read connection (`Shutdown` read half).
///
/// # Safety
///
/// `connection` must be a live channel connection from
/// `register_read_fd`.
pub unsafe fn close_read_connection(connection: *mut ngx::ffi::ngx_connection_t) {
    // SAFETY: caller guarantees a live channel connection.
    unsafe {
        ngx::ffi::ngx_close_connection(connection);
    }
}

/// Initializes the base-fetch channel (`NgxBaseFetch::Initialize`):
/// pipe, nonblocking ends, nginx registration, install, capacity
/// bump. Returns the read connection for `close_read_connection`,
/// or null (after logging) on failure.
///
/// # Safety
///
/// `cycle` must point at a live `ngx_cycle_t` in child init.
pub unsafe fn initialize_base_fetch_channel(
    cycle: *mut ngx::ffi::ngx_cycle_t,
) -> *mut ngx::ffi::ngx_connection_t {
    use ngx::ffi::NGX_LOG_EMERG;
    // SAFETY: caller guarantees a live cycle.
    unsafe {
        let log = (*cycle).log;
        let Some(fds) = open_pipe() else {
            ngx::ngx_log_error!(NGX_LOG_EMERG, log, "{}", PIPE_FAILED);
            return std::ptr::null_mut();
        };
        if !make_nonblocking(fds.read_fd) {
            ngx::ngx_log_error!(NGX_LOG_EMERG, log, "{}", PIPE_READ_NONBLOCKING_FAILED);
            close_fd(fds.read_fd);
            close_fd(fds.write_fd);
            return std::ptr::null_mut();
        }
        if !make_nonblocking(fds.write_fd) {
            ngx::ngx_log_error!(NGX_LOG_EMERG, log, "{}", PIPE_WRITE_NONBLOCKING_FAILED);
            close_fd(fds.read_fd);
            close_fd(fds.write_fd);
            return std::ptr::null_mut();
        }
        let connection = register_read_fd(cycle, fds.read_fd);
        if connection.is_null() {
            ngx::ngx_log_error!(NGX_LOG_EMERG, log, "{}", CONNECTION_FAILED);
            close_fd(fds.read_fd);
            close_fd(fds.write_fd);
            return std::ptr::null_mut();
        }
        let channel = PipeChannel::new(fds);
        bump_pipe_capacity(fds.write_fd);
        install_base_fetch_channel(channel);
        connection
    }
}

/// Pipe read-event handler (`ReadEventHandler`): re-arms, then
/// dispatches one event into the base-fetch read path.
///
/// # Safety
///
/// `ev` must point at a live pipe read event whose `data` is the
/// channel connection.
pub unsafe extern "C" fn base_fetch_read_event_handler(ev: *mut ngx::ffi::ngx_event_t) {
    use ngx::ffi::{ngx_handle_read_event, NGX_OK};
    // SAFETY: caller guarantees a live pipe read event.
    unsafe {
        let c = (*ev).data as *mut ngx::ffi::ngx_connection_t;
        let result = ngx_handle_read_event(ev, 0);
        assert!(
            result == NGX_OK as _,
            "pagespeed: ngx_handle_read_event error: {result}"
        );
        if (*ev).timedout() != 0 {
            (*ev).set_timedout(0);
            return;
        }
        // EOF/errors close via `Shutdown`, never here (the static
        // handler cannot null the owner's connection pointer).
        let mut dispatch = |event: PipeEvent| {
            // SAFETY: paired by the post; reclaimed exactly once.
            let fetch = crate::fetch::reclaim_event_sender(event);
            let r = fetch.with_state(|base| base.request());
            if r.is_null() {
                drop(fetch);
                return;
            }
            let detached = fetch.with_state(|base| base.detached());
            let zero = fetch.with_state(|base| base.decrement_ref_count());
            if zero || detached {
                drop(fetch);
                return;
            }
            let ctx = crate::request::get_request_ctx(r);
            if ctx.is_null() {
                let log = (*ngx::ffi::ngx_cycle).log;
                ngx::ngx_log_error!(
                    ngx::ffi::NGX_LOG_WARN,
                    log,
                    "pagespeed [{:p}] skipping event: request context gone",
                    r
                );
                drop(fetch);
                return;
            }
            let disassociated = match (*ctx).base_fetch.as_ref() {
                Some(current) => !std::ptr::eq(
                    std::sync::Arc::as_ptr(&fetch),
                    std::sync::Arc::as_ptr(current),
                ),
                None => true,
            };
            if disassociated {
                let log = (*ngx::ffi::ngx_cycle).log;
                ngx::ngx_log_error!(
                    ngx::ffi::NGX_LOG_WARN,
                    log,
                    "pagespeed [{:p}] skipping event: base fetch disassociated",
                    r
                );
                drop(fetch);
                return;
            }
            let upstream = (*r).upstream;
            let upstream_active = !upstream.is_null() && !(*upstream).cleanup.is_null();
            let fetch_type_is_html_or_ipro = fetch.with_state(|base| {
                matches!(
                    base.fetch_type(),
                    crate::fetch::BaseFetchType::HtmlTransform
                        | crate::fetch::BaseFetchType::IproLookup
                )
            });
            let connection_error_non_ipro =
                !fetch_type_is_html_or_ipro && (*(*r).connection).error() != 0;
            let rc = if connection_error_non_ipro {
                ngx::ffi::NGX_ERROR as ngx::ffi::ngx_int_t
            } else {
                // SAFETY: live request with attached context.
                crate::fetch::base_fetch_handler(r)
            };
            let _run_posted =
                match crate::fetch::read_callback_plan(&crate::fetch::ReadCallbackInputs {
                    refcount_reached_zero: zero,
                    detached,
                    ctx_gone: false,
                    disassociated: false,
                    connection_error_non_ipro,
                    handler_rc: rc,
                    upstream_active,
                }) {
                    crate::fetch::ReadCallbackAction::Skip => false,
                    crate::fetch::ReadCallbackAction::Finalize {
                        rc,
                        hold_upstream_ref,
                        run_posted,
                    } => {
                        if hold_upstream_ref {
                            let main = (*r).main;
                            (*main).set_count((*main).count() + 1);
                        }
                        ngx::ffi::ngx_http_finalize_request(r, rc);
                        run_posted
                    }
                    crate::fetch::ReadCallbackAction::UpstreamAgain { run_posted } => run_posted,
                };
            if _run_posted {
                // SAFETY: live request connection (the pipe's own
                // connection carries no posted requests).
                ngx::ffi::ngx_http_run_posted_requests((*r).connection);
            }
            drop(fetch);
        };
        let _ = drain_fd((*c).fd, &mut dispatch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::{event_byte, EventChannel, FetchEvent};

    const TINY_BUDGET: WriteBackoff = WriteBackoff {
        max_retries: 2,
        initial_us: 100,
        max_us: 200,
    };

    fn open_test_pipe() -> (PipeChannel, PipeFds) {
        let fds = open_pipe().expect("pipe");
        assert!(make_nonblocking(fds.read_fd));
        assert!(make_nonblocking(fds.write_fd));
        (PipeChannel::new(fds), fds)
    }

    #[test]
    fn round_trip_preserves_type_and_sender() {
        let (channel, fds) = open_test_pipe();
        let mut marker = 0u32;
        let sender = &mut marker as *mut u32 as *mut c_void;
        assert_eq!(PipeWrite::Sent, channel.write_event(b'H', sender));
        let mut seen = Vec::new();
        assert!(matches!(
            channel.drain(&mut |event| seen.push(event)),
            DrainOutcome::Dispatched(_)
        ));
        assert_eq!(1, seen.len());
        assert_eq!(b'H', seen[0].event_type);
        assert_eq!(sender, seen[0].sender);
        assert!(matches!(
            channel.drain(&mut |_| panic!("drained once")),
            DrainOutcome::Empty
        ));
        channel.shutdown_write();
        // Draining past a closed write end reports EOF.
        assert!(matches!(
            channel.drain(&mut |_| panic!("nothing left")),
            DrainOutcome::Ended
        ));
        close_fd(fds.read_fd);
    }

    #[test]
    fn writes_fail_after_shutdown() {
        let (channel, fds) = open_test_pipe();
        channel.shutdown_write();
        channel.shutdown_write();
        assert_eq!(
            PipeWrite::Closed,
            channel.write_event(b'F', std::ptr::null_mut())
        );
        close_fd(fds.read_fd);
    }

    #[test]
    fn full_pipe_gives_up_on_budget() {
        // No capacity bump here: the default 64 KiB fills fast.
        let fds = open_pipe().expect("pipe");
        assert!(make_nonblocking(fds.write_fd));
        let filler = [0u8; 4096];
        loop {
            // SAFETY: bulk fill until the pipe refuses.
            let wrote = unsafe {
                libc::write(fds.write_fd, filler.as_ptr() as *const c_void, filler.len())
            };
            if wrote < 0 {
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::EAGAIN)
                );
                break;
            }
        }
        let channel = PipeChannel::new(fds);
        let started = std::time::Instant::now();
        assert_eq!(
            PipeWrite::Full,
            channel.write_event_with_backoff(b'D', std::ptr::null_mut(), TINY_BUDGET)
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        // Draining a little room lets the next write through.
        let mut drained = 0;
        while drained < 4096 {
            let mut byte = [0u8; 1024];
            // SAFETY: scratch drain to free space.
            let got = unsafe { libc::read(fds.read_fd, byte.as_mut_ptr() as *mut c_void, 1024) };
            assert!(got > 0);
            drained += got;
        }
        assert_eq!(
            PipeWrite::Sent,
            channel.write_event_with_backoff(b'D', std::ptr::null_mut(), TINY_BUDGET)
        );
        channel.shutdown_write();
        close_fd(fds.read_fd);
    }

    #[test]
    fn slot_install_takes_and_posts() {
        use std::sync::Mutex;
        let slot: Mutex<Option<PipeChannel>> = Mutex::new(None);
        // Missing channel drops like the C++ null.
        let mut post = PipePost::new(&slot, std::ptr::null_mut());
        assert!(!post.write_event(FetchEvent::Flush));
        let (channel, fds) = open_test_pipe();
        install_channel_into(&slot, channel);
        let mut marker = 0u32;
        let sender = &mut marker as *mut u32 as *mut c_void;
        let mut post = PipePost::new(&slot, sender);
        assert!(post.write_event(FetchEvent::Done));
        let channel = take_channel_from(&slot).expect("installed");
        assert!(take_channel_from(&slot).is_none());
        let mut seen = Vec::new();
        assert!(matches!(
            channel.drain(&mut |event| seen.push(event)),
            DrainOutcome::Dispatched(_)
        ));
        assert_eq!(event_byte(FetchEvent::Done), seen[0].event_type);
        channel.shutdown_write();
        close_fd(fds.read_fd);
    }

    #[test]
    #[should_panic(expected = "event connection already set")]
    fn double_install_panics() {
        use std::sync::Mutex;
        let slot: Mutex<Option<PipeChannel>> = Mutex::new(None);
        let fds = open_pipe().expect("pipe");
        install_channel_into(&slot, PipeChannel::new(fds));
        let fds2 = open_pipe().expect("pipe");
        install_channel_into(&slot, PipeChannel::new(fds2));
    }

    #[test]
    fn terminate_plan() {
        assert_eq!(
            "NgxBaseFetch::Terminate rounding up 3 active base fetches.",
            terminate_roundup_message(3)
        );
        assert_eq!(
            "NgxBaseFetch::Terminate timed out with 2 active base fetches.",
            terminate_timeout_message(2)
        );
        assert_eq!(TERMINATE_TIMEOUT_US, 30_000_000);
        assert_eq!(TerminateStep::Done, terminate_step(0, 100, 200));
        assert_eq!(TerminateStep::KeepDraining, terminate_step(1, 100, 200));
        assert_eq!(TerminateStep::TimedOut, terminate_step(1, 200, 200));
        assert_eq!(TerminateStep::TimedOut, terminate_step(1, 300, 200));
        // Hitting zero exactly at the deadline still finishes clean.
        assert_eq!(TerminateStep::Done, terminate_step(0, 300, 200));
    }

    #[test]
    fn init_texts_verbatim() {
        assert_eq!(PIPE_FAILED, "pagespeed: pipe() failed");
        assert_eq!(
            PIPE_READ_NONBLOCKING_FAILED,
            "fcntl(O_NONBLOCK)pagespeed:  pipe[0] failed"
        );
        assert_eq!(
            PIPE_WRITE_NONBLOCKING_FAILED,
            "fcntl(O_NONBLOCK)pagespeed:  pipe[1] failed"
        );
        assert_eq!(CONNECTION_FAILED, "pagespeed: failed to create connection.");
    }
}
