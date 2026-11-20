//! Alarm scheduler (`Scheduler`, `SchedulerThread`, timer).
//!
//! Ports the scheduler core from
//! `pagespeed/kernel/thread/scheduler.{h,cc}`: absolute-time
//! alarms ordered by (deadline, insertion index), cancellation,
//! the signal/broadcast wakeup, and the scheduler-thread pump
//! loop. The condvar-wait family (`TimedWaitMs`,
//! `BlockingTimedWaitUs`, `SchedulerBlockingFunction`) and
//! `NewSequence` have no users on the nginx path (the port's
//! drivers are synchronous) and land with their first caller;
//! with no waiters the reduced `signal` below is exactly the
//! C++ `Signal`. `AddAlarmAtUsMutexHeld` likewise waits for the
//! 6d lock manager, its only outside user.
//!
//! Locking mirrors C++: one mutex guards the alarm sets, user
//! callbacks run with the lock dropped, and the pump checks the
//! stop flag and enters its wait in a single critical section
//! (like the C++ loop-top check under its held lock), so a stop
//! request can never miss the pump. Alarm order keys use `u64`
//! indices where C++ uses `uint32` (no wraparound; unobservable
//! below 2^32 alarms).

use ps_cache::timer::{MS_US, SECOND_US};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Pump idle bound (`255 * Timer::kSecondUs`, `SchedulerThread::Run`).
pub const SCHEDULER_PUMP_TIMEOUT_US: i64 = 255 * SECOND_US;

/// Microsecond clock (`Timer`, injected like the C++ `Timer*`).
pub trait Clock: Send + Sync + std::fmt::Debug {
    /// Current wall time in epoch microseconds (`NowUs`).
    fn now_us(&self) -> i64;
}

/// System wall clock (`PosixTimer::NowUs`).
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_us(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros()
            .min(i64::MAX as u128) as i64
    }
}

/// Manual test clock (advances only when told).
#[derive(Debug)]
pub struct ManualClock {
    now_us: Mutex<i64>,
}

impl ManualClock {
    /// Builds a clock reading `start_us`.
    #[must_use]
    pub fn new(start_us: i64) -> Self {
        ManualClock {
            now_us: Mutex::new(start_us),
        }
    }

    /// Advances the clock.
    pub fn advance_us(&self, delta_us: i64) {
        *self.now_us.lock().unwrap_or_else(|e| e.into_inner()) += delta_us;
    }
}

impl Clock for ManualClock {
    fn now_us(&self) -> i64 {
        *self.now_us.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Alarm callback pair (`Function` run/cancel halves; both run at
/// most once, without the scheduler lock).
pub struct AlarmCallback {
    /// Timeout body (`CallRun`).
    pub run: Box<dyn FnOnce() + Send>,
    /// Cancellation body (`CallCancel`).
    pub cancel: Box<dyn FnOnce() + Send>,
}

impl std::fmt::Debug for AlarmCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlarmCallback").finish_non_exhaustive()
    }
}

impl AlarmCallback {
    /// Builds a callback pair.
    #[must_use]
    pub fn new(
        run: impl FnOnce() + Send + 'static,
        cancel: impl FnOnce() + Send + 'static,
    ) -> Self {
        AlarmCallback {
            run: Box::new(run),
            cancel: Box::new(cancel),
        }
    }

    /// Builds a run-only callback (no-op cancel, like the default
    /// `Function::Cancel`).
    #[must_use]
    pub fn run_only(run: impl FnOnce() + Send + 'static) -> Self {
        AlarmCallback::new(run, || {})
    }
}

/// Alarm handle (the scheduler's insertion index, unique per scheduler).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AlarmId(u64);

/// One outstanding alarm.
#[derive(Debug)]
struct Alarm {
    id: AlarmId,
    callback: AlarmCallback,
}

/// Locked scheduler state.
#[derive(Debug)]
struct SchedulerState {
    /// Alarms keyed by (deadline, index).
    alarms: BTreeMap<(i64, u64), Alarm>,
    /// Alarm id to order key (for cancellation).
    wakeups: HashMap<AlarmId, (i64, u64)>,
    /// Last assigned index (starts at 0; first alarm is 1).
    next_index: u64,
    /// Stop requested (`SchedulerThread::quit_`, guarded here so
    /// the pump's check-then-wait is atomic).
    quit: bool,
}

/// Alarm scheduler (`Scheduler`, alarm subset).
#[derive(Debug)]
pub struct Scheduler {
    state: Mutex<SchedulerState>,
    wake: Condvar,
    clock: Arc<dyn Clock>,
}

impl Scheduler {
    /// Creates a scheduler on the system clock.
    #[must_use]
    pub fn new() -> Self {
        Scheduler::with_clock(Arc::new(SystemClock))
    }

    /// Creates a scheduler on an injected clock (tests).
    #[must_use]
    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Scheduler {
            state: Mutex::new(SchedulerState {
                alarms: BTreeMap::new(),
                wakeups: HashMap::new(),
                next_index: 0,
                quit: false,
            }),
            wake: Condvar::new(),
            clock,
        }
    }

    /// Current time in epoch microseconds.
    #[must_use]
    pub fn now_us(&self) -> i64 {
        self.clock.now_us()
    }

    /// Locks the alarm sets (recovers from poisoning: callbacks
    /// run unlocked, so poisoning implies a logic bug elsewhere,
    /// and the pump must stay alive regardless).
    fn lock_state(&self) -> MutexGuard<'_, SchedulerState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Schedules an alarm for an absolute time
    /// (`AddAlarmAtUs`): inserts, wakes the pump when the new
    /// alarm is earliest, then runs due alarms.
    pub fn add_alarm_at_us(&self, wakeup_time_us: i64, callback: AlarmCallback) -> AlarmId {
        let id = {
            let mut state = self.lock_state();
            state.next_index += 1;
            let id = AlarmId(state.next_index);
            let earliest_changed = state.alarms.is_empty()
                || wakeup_time_us < state.alarms.iter().next().expect("nonempty").0 .0;
            if earliest_changed {
                self.wake.notify_all();
            }
            state
                .alarms
                .insert((wakeup_time_us, id.0), Alarm { id, callback });
            state.wakeups.insert(id, (wakeup_time_us, id.0));
            id
        };
        self.run_alarms();
        id
    }

    /// Cancels an alarm (`CancelAlarm`): runs the cancel half
    /// without the lock. Returns false when the alarm already
    /// ran or was cancelled.
    pub fn cancel_alarm(&self, id: AlarmId) -> bool {
        let alarm = {
            let mut state = self.lock_state();
            match state.wakeups.remove(&id) {
                Some(key) => state.alarms.remove(&key).expect("sets agree"),
                None => return false,
            }
        };
        (alarm.callback.cancel)();
        true
    }

    /// Runs due alarms (`RunAlarms`): returns the next deadline,
    /// or 0 when none remain.
    pub fn run_alarms(&self) -> i64 {
        let (_state, _, next) = self.run_alarms_locked(self.lock_state());
        next
    }

    /// Runs due alarms with the lock held across the scan
    /// (dropped around each callback, like C++).
    fn run_alarms_locked<'s>(
        &'s self,
        mut state: MutexGuard<'s, SchedulerState>,
    ) -> (MutexGuard<'s, SchedulerState>, bool, i64) {
        let mut ran = false;
        loop {
            let key = match state.alarms.iter().next() {
                None => return (state, ran, 0),
                Some((key, _)) => *key,
            };
            if self.clock.now_us() < key.0 {
                return (state, ran, key.0);
            }
            let alarm = state.alarms.remove(&key).expect("peeked above");
            state.wakeups.remove(&alarm.id);
            let callback = alarm.callback;
            drop(state);
            (callback.run)();
            ran = true;
            state = self.lock_state();
        }
    }

    /// Wakes the pump and runs due alarms (`Signal` without the
    /// waiter dispatch, which has no users yet).
    pub fn signal(&self) {
        self.wake.notify_all();
        self.run_alarms();
    }

    /// Handles outstanding alarms, or waits until the next
    /// wakeup before relinquishing control
    /// (`ProcessAlarmsOrWaitUs`). Idles no longer than
    /// `timeout_us` (0 runs without blocking). Returns whether
    /// activities remain, runnable now or in the future.
    pub fn process_alarms_or_wait_us(&self, timeout_us: i64) -> bool {
        let (state, _) = self.process_locked(self.lock_state(), timeout_us);
        let pending = !state.alarms.is_empty();
        drop(state);
        pending
    }

    /// Locked `ProcessAlarmsOrWaitUs` core (shared with the pump
    /// so its quit check and wait stay atomic).
    fn process_locked<'s>(
        &'s self,
        state: MutexGuard<'s, SchedulerState>,
        timeout_us: i64,
    ) -> (MutexGuard<'s, SchedulerState>, bool) {
        let finish_us = self.clock.now_us().saturating_add(timeout_us);
        let (mut state, ran, mut next_wakeup_us) = self.run_alarms_locked(state);
        if timeout_us > 0 && !ran {
            if next_wakeup_us == 0 || next_wakeup_us > finish_us {
                next_wakeup_us = finish_us;
            }
            state = self.await_wakeup_until_us(state, next_wakeup_us);
            let (state_after, _, _) = self.run_alarms_locked(state);
            state = state_after;
        }
        let pending = !state.alarms.is_empty();
        (state, pending)
    }

    /// Waits until an absolute time or a wakeup
    /// (`AwaitWakeupUntilUs`), rounding the wait up to whole
    /// milliseconds like C++ (which may overshoot the deadline).
    fn await_wakeup_until_us<'s>(
        &'s self,
        state: MutexGuard<'s, SchedulerState>,
        wakeup_time_us: i64,
    ) -> MutexGuard<'s, SchedulerState> {
        let now_us = self.clock.now_us();
        if wakeup_time_us > now_us {
            // Ceiling division, verbatim (saturating: C++ would
            // overflow near `i64::MAX`).
            let wait_ms = wakeup_time_us
                .saturating_sub(now_us)
                .saturating_add(MS_US - 1)
                / MS_US;
            let (state, _) = self
                .wake
                .wait_timeout(state, Duration::from_millis(wait_ms as u64))
                .unwrap_or_else(|e| e.into_inner());
            return state;
        }
        state
    }

    /// Requests the pump loop to stop (the deleter's quit flag).
    pub fn request_stop(&self) {
        self.lock_state().quit = true;
        self.wake.notify_all();
    }

    /// Runs one pump iteration (`SchedulerThread::Run` body):
    /// returns true once a stop was requested.
    pub fn pump_once_us(&self, timeout_us: i64) -> bool {
        let state = self.lock_state();
        if state.quit {
            return true;
        }
        let _ = self.process_locked(state, timeout_us);
        false
    }

    /// Pumps until a stop is requested (`SchedulerThread::Run`).
    pub fn pump_until_stopped(&self) {
        while !self.pump_once_us(SCHEDULER_PUMP_TIMEOUT_US) {}
    }

    /// Whether any alarms remain (test/diagnostic helper; C++
    /// keeps `NoPendingAlarms` for its own tests too).
    #[cfg(test)]
    fn has_pending_alarms(&self) -> bool {
        !self.lock_state().alarms.is_empty()
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Scheduler::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn manual(start_us: i64) -> (Arc<ManualClock>, Scheduler) {
        let clock = Arc::new(ManualClock::new(start_us));
        let scheduler = Scheduler::with_clock(clock.clone());
        (clock, scheduler)
    }

    #[test]
    fn alarms_run_in_deadline_then_insertion_order() {
        let (clock, scheduler) = manual(1_000_000);
        let fired = Arc::new(Mutex::new(Vec::new()));
        for (tag, at) in [
            ("late", 1_000_100),
            ("early", 1_000_010),
            ("tie2", 1_000_050),
        ] {
            let fired = Arc::clone(&fired);
            scheduler.add_alarm_at_us(
                at,
                AlarmCallback::run_only(move || {
                    fired.lock().unwrap().push(tag);
                }),
            );
        }
        // Same deadline as tie2 but inserted later: runs after it.
        let fired_late = Arc::clone(&fired);
        scheduler.add_alarm_at_us(
            1_000_050,
            AlarmCallback::run_only(move || {
                fired_late.lock().unwrap().push("tie3");
            }),
        );
        assert!(scheduler.has_pending_alarms());
        clock.advance_us(200);
        assert_eq!(0, scheduler.run_alarms());
        assert!(!scheduler.has_pending_alarms());
        assert_eq!(
            vec!["early", "tie2", "tie3", "late"],
            *fired.lock().unwrap()
        );
    }

    #[test]
    fn past_deadline_runs_on_add_and_reports_next() {
        let (clock, scheduler) = manual(1_000_000);
        let fired = Arc::new(AtomicUsize::new(0));
        let probe = Arc::clone(&fired);
        // Future alarm first: `run_alarms` reports its deadline.
        scheduler.add_alarm_at_us(
            1_000_100,
            AlarmCallback::run_only(move || {
                probe.fetch_add(1, Ordering::SeqCst);
            }),
        );
        assert_eq!(0, fired.load(Ordering::SeqCst));
        assert_eq!(1_000_100, scheduler.run_alarms());
        // Past alarm runs synchronously inside the add.
        let probe = Arc::clone(&fired);
        scheduler.add_alarm_at_us(
            999_999,
            AlarmCallback::run_only(move || {
                probe.fetch_add(10, Ordering::SeqCst);
            }),
        );
        assert_eq!(10, fired.load(Ordering::SeqCst));
        clock.advance_us(200);
        assert_eq!(0, scheduler.run_alarms());
        assert_eq!(11, fired.load(Ordering::SeqCst));
    }

    #[test]
    fn cancel_runs_cancel_half_once() {
        let (_clock, scheduler) = manual(1_000_000);
        let ran = Arc::new(AtomicUsize::new(0));
        let cancelled = Arc::new(AtomicUsize::new(0));
        let run_probe = Arc::clone(&ran);
        let cancel_probe = Arc::clone(&cancelled);
        let id = scheduler.add_alarm_at_us(
            2_000_000,
            AlarmCallback::new(
                move || {
                    run_probe.fetch_add(1, Ordering::SeqCst);
                },
                move || {
                    cancel_probe.fetch_add(1, Ordering::SeqCst);
                },
            ),
        );
        assert!(scheduler.cancel_alarm(id));
        assert_eq!(0, ran.load(Ordering::SeqCst));
        assert_eq!(1, cancelled.load(Ordering::SeqCst));
        // Second cancel fails; unknown ids fail too.
        assert!(!scheduler.cancel_alarm(id));
        assert!(!scheduler.cancel_alarm(AlarmId(999)));
        assert!(!scheduler.has_pending_alarms());
    }

    #[test]
    fn cancel_after_run_fails() {
        let (_clock, scheduler) = manual(1_000_000);
        let id = scheduler.add_alarm_at_us(999_999, AlarmCallback::run_only(|| {}));
        assert!(!scheduler.cancel_alarm(id));
    }

    #[test]
    fn process_reports_pending_and_skips_blocking_at_zero() {
        let (_clock, scheduler) = manual(1_000_000);
        assert!(!scheduler.process_alarms_or_wait_us(0));
        scheduler.add_alarm_at_us(2_000_000, AlarmCallback::run_only(|| {}));
        // Zero timeout never blocks, even with future work pending.
        assert!(scheduler.process_alarms_or_wait_us(0));
        assert!(scheduler.has_pending_alarms());
    }

    #[test]
    fn signal_runs_due_alarms() {
        let (clock, scheduler) = manual(1_000_000);
        let fired = Arc::new(AtomicUsize::new(0));
        let probe = Arc::clone(&fired);
        scheduler.add_alarm_at_us(
            1_000_100,
            AlarmCallback::run_only(move || {
                probe.fetch_add(1, Ordering::SeqCst);
            }),
        );
        clock.advance_us(200);
        scheduler.signal();
        assert_eq!(1, fired.load(Ordering::SeqCst));
    }

    #[test]
    fn earlier_alarm_wakes_a_waiting_pump() {
        let scheduler = Arc::new(Scheduler::new());
        let fired = Arc::new(AtomicUsize::new(0));
        let probe = Arc::clone(&fired);
        // Far-future alarm parks the pump in a long wait.
        scheduler.add_alarm_at_us(
            scheduler.now_us() + 60 * SECOND_US,
            AlarmCallback::run_only(|| {}),
        );
        let pump = {
            let scheduler = Arc::clone(&scheduler);
            std::thread::spawn(move || scheduler.pump_until_stopped())
        };
        // Give the pump a moment to park, then insert a near alarm:
        // the earliest-change broadcast must wake it promptly.
        std::thread::sleep(Duration::from_millis(100));
        scheduler.add_alarm_at_us(
            scheduler.now_us() + 20_000,
            AlarmCallback::run_only(move || {
                probe.fetch_add(1, Ordering::SeqCst);
            }),
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while fired.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(1, fired.load(Ordering::SeqCst));
        scheduler.request_stop();
        pump.join().expect("pump joins");
    }

    #[test]
    fn pump_exits_promptly_on_stop() {
        let scheduler = Arc::new(Scheduler::new());
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let pump = {
            let scheduler = Arc::clone(&scheduler);
            std::thread::spawn(move || {
                scheduler.pump_until_stopped();
                done_tx.send(()).expect("report exit");
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        scheduler.request_stop();
        done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("pump exits promptly");
        pump.join().expect("pump joins");
        // A stopped scheduler reports quit immediately.
        assert!(scheduler.pump_once_us(0));
    }

    #[test]
    fn system_clock_reads_epoch_microseconds() {
        let now = SystemClock.now_us();
        assert!(now > 1_700_000_000_000_000, "implausible epoch us: {now}");
        let expected = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_micros() as i64;
        assert!(
            (now - expected).abs() < SECOND_US,
            "clock jumped: {now} vs {expected}"
        );
    }
}
