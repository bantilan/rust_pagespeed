//! Thread-safe statistics primitives and registry.
//!
//! Ports `pagespeed/kernel/base/statistics.h` (`Variable`,
//! `UpDownCounter`, `Histogram`, `TimedVariable`, `Statistics`) with the
//! bucket layout and algorithms of
//! `pagespeed/kernel/sharedmem/shared_mem_statistics.cc`
//! (`SharedMemHistogram`).
//!
//! Memory model: counters are lock-free atomics (`Relaxed` ordering suits
//! monotonic reporting counters); histograms and timed variables hold a
//! mutex, as in C++. A registry built with [`Statistics::with_shared`]
//! backs plain and up/down counters in a cross-process memory-mapped
//! table (`ps-shm`), so every nginx worker reports global totals;
//! histograms and timed variables stay per-process by design, and a
//! plain [`Statistics::new`] registry stays fully in-process.
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Counter cell: a process-local atomic or a shared-memory slot.
#[derive(Debug)]
enum Cell {
    /// In-process atomic (plain registries and shared fallbacks).
    Local(AtomicI64),
    /// Cross-process slot (shared registries).
    Shared(SharedCell),
}

impl Default for Cell {
    fn default() -> Self {
        Cell::Local(AtomicI64::new(0))
    }
}

/// Shared-memory cell: the mapped table plus the slot index.
#[derive(Debug, Clone)]
struct SharedCell {
    counters: Arc<ps_shm::SharedCounters>,
    slot: usize,
}

/// Monotonic counter (`Variable`): `Get`, `Add` (non-negative), `Clear`.
#[derive(Debug, Default)]
pub struct Variable {
    cell: Cell,
    name: String,
}

impl Variable {
    /// Creates a zeroed variable.
    #[must_use]
    pub fn new(name: &str) -> Variable {
        Variable {
            cell: Cell::Local(AtomicI64::new(0)),
            name: name.to_string(),
        }
    }

    /// Creates a variable over a shared slot.
    pub(crate) fn shared(
        name: &str,
        counters: Arc<ps_shm::SharedCounters>,
        slot: usize,
    ) -> Variable {
        Variable {
            cell: Cell::Shared(SharedCell { counters, slot }),
            name: name.to_string(),
        }
    }

    /// Returns the name (`GetName`).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the current value (`Get`).
    #[must_use]
    pub fn get(&self) -> i64 {
        match &self.cell {
            Cell::Local(value) => value.load(Ordering::Relaxed),
            Cell::Shared(cell) => cell.counters.get(cell.slot),
        }
    }

    /// Adds a non-negative delta, returning the result (`Add`).
    ///
    /// Like C++ (`DCHECK_LE`), negativity is a debug-only assertion;
    /// release wraps.
    pub fn add(&self, delta: i64) -> i64 {
        debug_assert!(delta >= 0, "Variable::add requires non-negative delta");
        match &self.cell {
            Cell::Local(value) => value.fetch_add(delta, Ordering::Relaxed) + delta,
            Cell::Shared(cell) => cell.counters.add(cell.slot, delta),
        }
    }

    /// Resets to zero (`Clear`).
    pub fn clear(&self) {
        match &self.cell {
            Cell::Local(value) => value.store(0, Ordering::Relaxed),
            Cell::Shared(cell) => cell.counters.clear(cell.slot),
        }
    }
}

/// Bidirectional counter (`UpDownCounter`): `Get`, `Set`,
/// `SetReturningPreviousValue`, `Add`, `Clear`.
#[derive(Debug, Default)]
pub struct UpDownCounter {
    cell: Cell,
    name: String,
}

impl UpDownCounter {
    /// Creates a zeroed counter.
    #[must_use]
    pub fn new(name: &str) -> UpDownCounter {
        UpDownCounter {
            cell: Cell::Local(AtomicI64::new(0)),
            name: name.to_string(),
        }
    }

    /// Creates a counter over a shared slot.
    pub(crate) fn shared(
        name: &str,
        counters: Arc<ps_shm::SharedCounters>,
        slot: usize,
    ) -> UpDownCounter {
        UpDownCounter {
            cell: Cell::Shared(SharedCell { counters, slot }),
            name: name.to_string(),
        }
    }

    /// Returns the name (`GetName`).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the current value (`Get`).
    #[must_use]
    pub fn get(&self) -> i64 {
        match &self.cell {
            Cell::Local(value) => value.load(Ordering::Relaxed),
            Cell::Shared(cell) => cell.counters.get(cell.slot),
        }
    }

    /// Sets the value (`Set`).
    pub fn set(&self, value: i64) {
        match &self.cell {
            Cell::Local(current) => current.store(value, Ordering::Relaxed),
            Cell::Shared(cell) => cell.counters.set(cell.slot, value),
        }
    }

    /// Atomically sets the value, returning the previous one
    /// (`SetReturningPreviousValue`; C++ requires atomicity from
    /// implementations used for races, which this satisfies).
    pub fn set_returning_previous(&self, value: i64) -> i64 {
        match &self.cell {
            Cell::Local(current) => current.swap(value, Ordering::AcqRel),
            Cell::Shared(cell) => cell.counters.swap(cell.slot, value),
        }
    }

    /// Adds a (possibly negative) delta, returning the result (`Add`).
    pub fn add(&self, delta: i64) -> i64 {
        match &self.cell {
            Cell::Local(value) => value.fetch_add(delta, Ordering::Relaxed) + delta,
            Cell::Shared(cell) => cell.counters.add(cell.slot, delta),
        }
    }

    /// Resets to zero (`Clear`).
    pub fn clear(&self) {
        self.set(0);
    }
}

/// Default linear bucket count (`kDefaultNumBuckets`), excluding catchers.
pub const DEFAULT_NUM_BUCKETS: usize = 500;
/// Out-of-range catcher buckets (`kOutOfBoundsCatcherBuckets`).
pub const CATCHER_BUCKETS: usize = 2;
/// Default value ceiling (`kMaxValue`).
pub const DEFAULT_MAX_VALUE: f64 = 5000.0;

/// Bucketed histogram (`SharedMemHistogram` without the shared segment).
///
/// Layout: bucket 0 catches `(-inf, min)`, buckets `1..=n` tile
/// `[min, max)` linearly, bucket `n+1` catches `[max, +inf)`. With
/// negative buckets the range is `(-max, +max)`. Statistics
/// (average/percentile/stddev/count/min/max) follow the C++ algorithms,
/// including the interpolation bound `min(width, max - bucket_start)`.
/// Reconfiguring bounds or bucket count resets the data, as in C++.
#[derive(Debug)]
pub struct Histogram {
    name: String,
    state: Mutex<HistogramState>,
}

#[derive(Debug)]
struct HistogramState {
    buckets: Vec<f64>,
    enable_negative: bool,
    min_value: f64,
    max_value: f64,
    min: f64,
    max: f64,
    count: f64,
    sum: f64,
    sum_of_squares: f64,
}

impl HistogramState {
    fn new(num_buckets: usize) -> HistogramState {
        HistogramState {
            buckets: vec![0.0; num_buckets + CATCHER_BUCKETS],
            enable_negative: false,
            min_value: 0.0,
            max_value: DEFAULT_MAX_VALUE,
            min: 0.0,
            max: 0.0,
            count: 0.0,
            sum: 0.0,
            sum_of_squares: 0.0,
        }
    }

    fn reset_data(&mut self) {
        // `ClearInternal`: min/max reset to 0, not infinities.
        self.min = 0.0;
        self.max = 0.0;
        self.count = 0.0;
        self.sum = 0.0;
        self.sum_of_squares = 0.0;
        self.buckets.fill(0.0);
    }

    fn num_buckets(&self) -> usize {
        self.buckets.len()
    }

    fn width(&self) -> f64 {
        let linear = self.num_buckets() - CATCHER_BUCKETS;
        if self.enable_negative {
            self.max_value * 2.0 / linear as f64
        } else {
            (self.max_value - self.min_value) / linear as f64
        }
    }

    fn bucket_start(&self, index: usize) -> f64 {
        if index == self.num_buckets() {
            return f64::INFINITY;
        }
        if index == 0 {
            return f64::NEG_INFINITY;
        }
        let inner = index - 1;
        if self.enable_negative {
            inner as f64 * self.width() + -self.max_value
        } else {
            self.min_value + inner as f64 * self.width()
        }
    }

    fn find_bucket(&self, value: f64) -> usize {
        // `FindBucket`, skipping the left catcher with +1.
        let width = self.width();
        if self.enable_negative && value > 0.0 {
            let index_zero = self.find_bucket(0.0);
            let lower = self.bucket_start(index_zero);
            (index_zero as f64 + (value - lower) / width) as usize
        } else if self.enable_negative {
            (1.0 + (value + self.max_value) / width) as usize
        } else {
            (1.0 + (value - self.min_value) / width) as usize
        }
    }
}

impl Histogram {
    /// Creates an empty histogram with default bounds.
    #[must_use]
    pub fn new(name: &str) -> Histogram {
        Histogram {
            name: name.to_string(),
            state: Mutex::new(HistogramState::new(DEFAULT_NUM_BUCKETS)),
        }
    }

    /// Returns the name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    fn lock(&self) -> MutexGuard<'_, HistogramState> {
        self.state.lock().unwrap_or_else(|err| err.into_inner())
    }

    /// Records a value (`Add`).
    pub fn add(&self, value: f64) {
        let mut state = self.lock();
        let num = state.num_buckets();
        let index = if state.enable_negative {
            if value < -state.max_value {
                0
            } else if value >= state.max_value {
                num - 1
            } else {
                state.find_bucket(value)
            }
        } else if value < state.min_value {
            0
        } else if value >= state.max_value {
            num - 1
        } else {
            state.find_bucket(value)
        };
        if index >= num {
            return; // C++ logs and drops out-of-range indices.
        }
        state.buckets[index] += 1.0;
        if state.count == 0.0 {
            state.min = value;
            state.max = value;
        } else if value < state.min {
            state.min = value;
        } else if value > state.max {
            state.max = value;
        }
        state.count += 1.0;
        state.sum += value;
        state.sum_of_squares += value * value;
    }

    /// Throws away all data (`Clear`).
    pub fn clear(&self) {
        self.lock().reset_data();
    }

    /// Whether no values were recorded (`Empty`).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock().count == 0.0
    }

    /// Total bucket count including catchers (`NumBuckets`).
    #[must_use]
    pub fn num_buckets(&self) -> usize {
        self.lock().num_buckets()
    }

    /// Allows negative values (`EnableNegativeBuckets`; resets data).
    pub fn enable_negative_buckets(&self) {
        let mut state = self.lock();
        state.enable_negative = true;
        state.reset_data();
    }

    /// Sets the minimum in-range value (`SetMinValue`; resets data).
    pub fn set_min_value(&self, value: f64) {
        let mut state = self.lock();
        state.min_value = value;
        state.reset_data();
    }

    /// Sets the value ceiling (`SetMaxValue`; resets data).
    pub fn set_max_value(&self, value: f64) {
        let mut state = self.lock();
        state.max_value = value;
        state.reset_data();
    }

    /// Sets the linear bucket count (`SetSuggestedNumBuckets`; resets data).
    pub fn set_suggested_num_buckets(&self, count: usize) {
        let mut state = self.lock();
        let negative = state.enable_negative;
        let min = state.min_value;
        let max = state.max_value;
        *state = HistogramState::new(count);
        state.enable_negative = negative;
        state.min_value = min;
        state.max_value = max;
    }

    /// Average of recorded values (`Average`; 0 when empty).
    #[must_use]
    pub fn average(&self) -> f64 {
        let state = self.lock();
        if state.count == 0.0 {
            0.0
        } else {
            state.sum / state.count
        }
    }

    /// Estimated value above `perc`% of the data (`Percentile`).
    #[must_use]
    pub fn percentile(&self, perc: f64) -> f64 {
        let state = self.lock();
        if state.count == 0.0 || perc < 0.0 {
            return 0.0;
        }
        let count_below = (state.count * perc / 100.0).floor();
        let mut count = 0.0;
        let num = state.num_buckets();
        let mut i = 0;
        while i < num {
            if count + state.buckets[i] <= count_below {
                count += state.buckets[i];
                if count == count_below {
                    return state.bucket_start(i + 1);
                }
            } else {
                break;
            }
            i += 1;
        }
        if i >= num || state.buckets[i] == 0.0 {
            // Unreachable for perc in [0, 100]; guards debug builds.
            return state.max;
        }
        let fraction = (count_below + 1.0 - count) / state.buckets[i];
        let bound = state.width().min(state.max - state.bucket_start(i));
        state.bucket_start(i) + fraction * bound
    }

    /// Standard deviation (`StandardDeviationInternal`, with the epsilon
    /// guard that collapses float noise to 0).
    #[must_use]
    pub fn standard_deviation(&self) -> f64 {
        let state = self.lock();
        if state.count == 0.0 {
            return 0.0;
        }
        let variance = (state.sum_of_squares * state.count - state.sum * state.sum)
            / (state.count * state.count);
        if variance < state.sum_of_squares * f64::EPSILON {
            return 0.0;
        }
        variance.sqrt()
    }

    /// Number of recorded values (`Count`).
    #[must_use]
    pub fn count(&self) -> f64 {
        self.lock().count
    }

    /// Largest recorded value (`Maximum`; 0 when empty, as in C++).
    #[must_use]
    pub fn maximum(&self) -> f64 {
        self.lock().max
    }

    /// Smallest recorded value (`Minimum`; 0 when empty, as in C++).
    #[must_use]
    pub fn minimum(&self) -> f64 {
        self.lock().min
    }

    /// Median (`Percentile(50)`).
    #[must_use]
    pub fn median(&self) -> f64 {
        self.percentile(50.0)
    }

    /// Lower bound of a bucket (`BucketStart`).
    #[must_use]
    pub fn bucket_start(&self, index: usize) -> f64 {
        self.lock().bucket_start(index)
    }

    /// Upper bound of a bucket (`BucketLimit`).
    #[must_use]
    pub fn bucket_limit(&self, index: usize) -> f64 {
        self.lock().bucket_start(index + 1)
    }

    /// Count in a bucket (`BucketCount`).
    #[must_use]
    pub fn bucket_count(&self, index: usize) -> f64 {
        self.lock().buckets[index]
    }
}

/// Timed-variable window (`TimedVariable::Levels`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimedLevel {
    /// Last 10 seconds.
    TenSec,
    /// Last minute.
    Minute,
    /// Last hour.
    Hour,
    /// Since start (total).
    Start,
}

impl TimedLevel {
    /// Window length, if bounded.
    #[must_use]
    pub const fn window(self) -> Option<Duration> {
        match self {
            TimedLevel::TenSec => Some(Duration::from_secs(10)),
            TimedLevel::Minute => Some(Duration::from_secs(60)),
            TimedLevel::Hour => Some(Duration::from_secs(3600)),
            TimedLevel::Start => None,
        }
    }
}

/// Clock for timed variables (injectable for deterministic tests).
pub trait Clock: Send + Sync {
    /// Current time.
    fn now(&self) -> Instant;
}

impl<T: Clock + ?Sized> Clock for Arc<T> {
    fn now(&self) -> Instant {
        T::now(self)
    }
}

/// System clock.
#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Interval counter (`TimedVariable`): `IncBy`, `Get(level)`, `Clear`.
///
/// Keeps timestamped deltas (pruned past one hour on access) so each level
/// reports the amount added in its window. `Start` reports the lifetime
/// total.
#[derive(Debug)]
pub struct TimedVariable<C: Clock = SystemClock> {
    name: String,
    clock: C,
    state: Mutex<TimedState>,
}

#[derive(Debug, Default)]
struct TimedState {
    samples: std::collections::VecDeque<(Instant, i64)>,
    total: i64,
}

impl TimedVariable<SystemClock> {
    /// Creates a timed variable on the system clock.
    #[must_use]
    pub fn new(name: &str) -> TimedVariable<SystemClock> {
        TimedVariable::with_clock(name, SystemClock)
    }
}

impl<C: Clock> TimedVariable<C> {
    /// Creates a timed variable on an injected clock.
    #[must_use]
    pub fn with_clock(name: &str, clock: C) -> TimedVariable<C> {
        TimedVariable {
            name: name.to_string(),
            clock,
            state: Mutex::new(TimedState::default()),
        }
    }

    /// Returns the name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Adds a delta at the current time (`IncBy`).
    pub fn inc_by(&self, delta: i64) {
        let now = self.clock.now();
        let mut state = self.state.lock().unwrap_or_else(|err| err.into_inner());
        state.samples.push_back((now, delta));
        state.total += delta;
        Self::prune_locked(&mut state, now);
    }

    /// Amount added in the level's window (`Get`).
    #[must_use]
    pub fn get(&self, level: TimedLevel) -> i64 {
        let now = self.clock.now();
        let mut state = self.state.lock().unwrap_or_else(|err| err.into_inner());
        Self::prune_locked(&mut state, now);
        match level.window() {
            None => state.total,
            Some(window) => {
                let cutoff = now.checked_sub(window);
                state
                    .samples
                    .iter()
                    .filter(|(time, _)| cutoff.is_none_or(|cut| *time >= cut))
                    .map(|(_, delta)| *delta)
                    .sum()
            }
        }
    }

    /// Throws away all data (`Clear`).
    pub fn clear(&self) {
        let mut state = self.state.lock().unwrap_or_else(|err| err.into_inner());
        state.samples.clear();
        state.total = 0;
    }

    fn prune_locked(state: &mut TimedState, now: Instant) {
        // Samples older than the widest window (one hour) affect nothing.
        while let Some((time, _)) = state.samples.front() {
            if now.duration_since(*time) > Duration::from_secs(3600) {
                state.samples.pop_front();
            } else {
                break;
            }
        }
    }
}

/// Group tag for timed variables (`Statistics::kDefaultGroup`).
pub const DEFAULT_TIMED_GROUP: &str = "default";

/// Handle to a registered statistic.
#[derive(Debug, Clone)]
pub enum StatHandle {
    /// Plain counter.
    Variable(Arc<Variable>),
    /// Bidirectional counter (also used for global counters).
    UpDown(Arc<UpDownCounter>),
    /// Histogram.
    Histogram(Arc<Histogram>),
    /// Timed variable.
    Timed(Arc<TimedVariable>),
}

/// Named statistic registry (`Statistics`).
///
/// `Add*` methods return shared handles and are idempotent per name+kind:
/// re-adding an existing name returns the original handle, matching the
/// shared-memory implementation's attach semantics.
#[derive(Debug, Default)]
pub struct Statistics {
    variables: Mutex<HashMap<String, Arc<Variable>>>,
    up_downs: Mutex<HashMap<String, Arc<UpDownCounter>>>,
    histograms: Mutex<HashMap<String, Arc<Histogram>>>,
    timed: Mutex<HashMap<String, Arc<TimedVariable>>>,
    console_logger: Mutex<Option<Arc<dyn StatisticsLogger>>>,
    shared: Option<SharedBackend>,
    shared_fallbacks: AtomicI64,
}

/// Cross-process counter backend: the mapped table plus the fixed
/// name-to-slot index (dynamic names attach at first use).
#[derive(Debug, Clone)]
struct SharedBackend {
    counters: Arc<ps_shm::SharedCounters>,
    fixed: HashMap<String, usize>,
}

/// Statistics-log reader (`StatisticsLogger`): graph JSON dumps
/// and previous-instance counter recovery.
pub trait StatisticsLogger: Send + Sync + std::fmt::Debug {
    /// Dumps log samples as JSON (`DumpJSON`).
    fn dump_json(
        &self,
        for_graphs: bool,
        var_titles: &[String],
        start_ms: i64,
        end_ms: i64,
        granularity_ms: i64,
    ) -> String;

    /// Recovers previous-instance dashboard counters
    /// (`PreviousDashboardCounters`).
    fn previous_dashboard_counters(&self, out: &mut std::collections::HashMap<String, i64>);
}

impl Statistics {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Statistics {
        Statistics::default()
    }

    /// Creates a registry backed by a cross-process counter table.
    /// Pre-registered names map to fixed slots; names first seen at
    /// runtime attach to spare dynamic slots, falling back to
    /// process-local counters when the table is full (counted in
    /// [`Statistics::shared_fallbacks`]).
    #[must_use]
    pub fn with_shared(counters: Arc<ps_shm::SharedCounters>) -> Statistics {
        let fixed = counters
            .fixed_names()
            .iter()
            .enumerate()
            .map(|(index, name)| (name.clone(), index))
            .collect();
        Statistics {
            shared: Some(SharedBackend { counters, fixed }),
            ..Statistics::default()
        }
    }

    /// Attaches a name to a shared slot, if this registry is shared.
    fn shared_slot(&self, name: &str) -> Option<(Arc<ps_shm::SharedCounters>, usize)> {
        let backend = self.shared.as_ref()?;
        if let Some(slot) = backend.fixed.get(name) {
            return Some((Arc::clone(&backend.counters), *slot));
        }
        match backend.counters.attach_dynamic(name) {
            Some(slot) => Some((Arc::clone(&backend.counters), slot)),
            None => {
                self.shared_fallbacks.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    /// Counts names that fell back to local counters (dynamic table full
    /// or overlong names; zero in a healthy shared registry).
    #[must_use]
    pub fn shared_fallbacks(&self) -> i64 {
        self.shared_fallbacks.load(Ordering::Relaxed)
    }

    /// Lists every registered plain and up/down counter name, sorted
    /// (factory collects the fixed table from a scratch registry).
    #[must_use]
    pub fn registered_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        names.extend(
            self.variables
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .keys()
                .cloned(),
        );
        names.extend(
            self.up_downs
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .keys()
                .cloned(),
        );
        names.sort();
        names.dedup();
        names
    }

    /// Adds (or attaches to) a counter (`AddVariable`).
    pub fn add_variable(&self, name: &str) -> Arc<Variable> {
        // Fast path first: attaching allocates under a cross-process
        // spinlock, so it must run only for genuinely new names.
        if let Some(existing) = self.get_variable(name) {
            return existing;
        }
        // Shared counters share one slot namespace across kinds: a name
        // registered as both would alias. Statically disjoint today;
        // this fires in tests if that ever regresses.
        debug_assert!(
            self.get_up_down_counter(name).is_none(),
            "counter kind collision: {name}"
        );
        match self.shared_slot(name) {
            Some((counters, slot)) => insert_shared(&self.variables, name, || {
                Variable::shared(name, counters, slot)
            }),
            None => insert_shared(&self.variables, name, || Variable::new(name)),
        }
    }

    /// Adds (or attaches to) a bidirectional counter (`AddUpDownCounter`).
    pub fn add_up_down_counter(&self, name: &str) -> Arc<UpDownCounter> {
        if let Some(existing) = self.get_up_down_counter(name) {
            return existing;
        }
        debug_assert!(
            self.get_variable(name).is_none(),
            "counter kind collision: {name}"
        );
        match self.shared_slot(name) {
            Some((counters, slot)) => insert_shared(&self.up_downs, name, || {
                UpDownCounter::shared(name, counters, slot)
            }),
            None => insert_shared(&self.up_downs, name, || UpDownCounter::new(name)),
        }
    }

    /// Adds a process-global counter (`AddGlobalUpDownCounter`; the base
    /// implementation forwards to `AddVariable`-family storage, as here).
    pub fn add_global_up_down_counter(&self, name: &str) -> Arc<UpDownCounter> {
        self.add_up_down_counter(name)
    }

    /// Adds (or attaches to) a histogram (`AddHistogram`).
    pub fn add_histogram(&self, name: &str) -> Arc<Histogram> {
        insert_shared(&self.histograms, name, || Histogram::new(name))
    }

    /// Adds (or attaches to) a timed variable (`AddTimedVariable`).
    pub fn add_timed_variable(&self, name: &str, _group: &str) -> Arc<TimedVariable> {
        insert_shared(&self.timed, name, || TimedVariable::new(name))
    }

    /// Looks up a counter (`GetVariable`; `None` when absent).
    #[must_use]
    pub fn get_variable(&self, name: &str) -> Option<Arc<Variable>> {
        self.variables
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .get(name)
            .cloned()
    }

    /// Looks up a histogram (`FindHistogram`-family; `None` when absent).
    #[must_use]
    pub fn get_histogram(&self, name: &str) -> Option<Arc<Histogram>> {
        self.histograms
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .get(name)
            .cloned()
    }

    /// Looks up an up/down counter (`None` when absent).
    #[must_use]
    pub fn get_up_down_counter(&self, name: &str) -> Option<Arc<UpDownCounter>> {
        self.up_downs
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .get(name)
            .cloned()
    }

    /// Looks up a timed variable (`None` when absent).
    #[must_use]
    pub fn get_timed_variable(&self, name: &str) -> Option<Arc<TimedVariable>> {
        self.timed
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .get(name)
            .cloned()
    }

    /// Installs the console logger (`set_console_logger`).
    pub fn set_console_logger(&self, logger: Arc<dyn StatisticsLogger>) {
        *self
            .console_logger
            .lock()
            .unwrap_or_else(|err| err.into_inner()) = Some(logger);
    }

    /// Borrows the console logger, if any (`console_logger`).
    #[must_use]
    pub fn console_logger(&self) -> Option<Arc<dyn StatisticsLogger>> {
        self.console_logger
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clone()
    }

    /// Dumps `name: value` lines, value-aligned (`Dump`). Names
    /// sort alphabetically (C++ keeps creation order, which
    /// this map does not record).
    #[must_use]
    pub fn dump(&self) -> String {
        let mut entries = self.counter_entries();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        let longest = entries
            .iter()
            .map(|(name, value)| name.len() + value.len())
            .max()
            .unwrap_or(0);
        let mut out = String::new();
        for (name, value) in entries {
            let padding = longest - name.len() - value.len();
            out.push_str(&format!("{name}: {}{value}\n", " ".repeat(padding)));
        }
        out
    }

    /// Dumps counters as JSON (`DumpJson`): variables then
    /// up-downs, each sorted (C++ keeps creation order).
    #[must_use]
    pub fn dump_json(&self) -> String {
        let (variables, up_downs) = self.sorted_counter_lists();
        let longest = variables
            .iter()
            .chain(up_downs.iter())
            .map(|(name, value)| name.len() + value.len())
            .max()
            .unwrap_or(0);
        let mut out = String::from("{\"variables\": {");
        for (index, (name, value)) in variables.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str(&format!("\"{name}\": {value}"));
        }
        for (name, value) in &up_downs {
            out.push(',');
            out.push_str(&format!("\"{name}\": {value}"));
        }
        out.push_str(&format!("}}, \"maxlength\": {longest}}}"));
        out
    }

    /// Collects counter name/value pairs in map order.
    fn counter_entries(&self) -> Vec<(String, String)> {
        let (variables, up_downs) = self.sorted_counter_lists();
        let mut entries = variables;
        entries.extend(up_downs);
        entries
    }

    /// Sorted variable and up-down lists.
    fn sorted_counter_lists(&self) -> (Vec<(String, String)>, Vec<(String, String)>) {
        let mut variables: Vec<(String, String)> = self
            .variables
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .iter()
            .map(|(name, var)| (name.clone(), var.get().to_string()))
            .collect();
        variables.sort_by(|left, right| left.0.cmp(&right.0));
        let mut up_downs: Vec<(String, String)> = self
            .up_downs
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .iter()
            .map(|(name, var)| (name.clone(), var.get().to_string()))
            .collect();
        up_downs.sort_by(|left, right| left.0.cmp(&right.0));
        (variables, up_downs)
    }

    /// Renders populated histograms (`RenderHistograms`).
    #[must_use]
    pub fn render_histograms(&self) -> String {
        let mut names: Vec<String> = self
            .histograms
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .keys()
            .cloned()
            .collect();
        names.sort();
        let mut populated = Vec::new();
        for name in &names {
            if let Some(hist) = self.get_histogram(name) {
                if hist.count() != 0.0 {
                    populated.push((name.clone(), hist));
                }
            }
        }
        let mut out = String::from("<hr/>");
        if populated.is_empty() {
            out.push_str(
                "<em>No histogram data yet.  Refresh once there is traffic.</em>",
            );
        } else {
            out.push_str(HISTOGRAM_PROLOG);
            for (index, (name, hist)) in populated.iter().enumerate() {
                out.push_str(&histogram_table_row(name, index, hist));
            }
            out.push_str(HISTOGRAM_EPILOG);
            for (index, (_, hist)) in populated.iter().enumerate() {
                out.push_str(&format!("<div id='hist_{index}' style='display:none'>"));
                out.push_str(&histogram_buckets(hist));
                out.push_str("</div>\n");
            }
            out.push_str(HISTOGRAM_SCRIPT);
        }
        out.push_str("<hr/>\n");
        out
    }

    /// Snapshots all plain and up/down counters by name (for the logger).
    #[must_use]
    pub fn snapshot_counters(&self) -> HashMap<String, i64> {
        let mut out = HashMap::new();
        for (name, var) in self
            .variables
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .iter()
        {
            out.insert(name.clone(), var.get());
        }
        for (name, var) in self
            .up_downs
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .iter()
        {
            out.insert(name.clone(), var.get());
        }
        out
    }
}

fn insert_shared<T>(
    map: &Mutex<HashMap<String, Arc<T>>>,
    name: &str,
    make: impl FnOnce() -> T,
) -> Arc<T> {
    let mut map = map.lock().unwrap_or_else(|err| err.into_inner());
    if let Some(existing) = map.get(name) {
        return existing.clone();
    }
    let handle = Arc::new(make());
    map.insert(name.to_string(), handle.clone());
    handle
}

/// Histogram table prolog (`kHistogramProlog`).
const HISTOGRAM_PROLOG: &str = "<div class='pagespeed-admin-table-wrap'>\n  <table class='pagespeed-admin-data-table pagespeed-histogram-table'>\n    <thead><tr>\n      <th>Histogram</th>\n      <th>Count</th>\n      <th>Average</th>\n      <th>Std. Dev.</th>\n      <th>Min</th>\n      <th>Median</th>\n      <th>Max</th>\n      <th>90th</th>\n      <th>95th</th>\n      <th>99th</th>\n    </tr></thead><tbody>\n";

/// Histogram table epilog (`kHistogramEpilog`).
const HISTOGRAM_EPILOG: &str = "    </tbody>\n  </table>\n</div>\n";

/// Histogram interactivity (`kHistogramScript`).
const HISTOGRAM_SCRIPT: &str = "<script>\n  var currentHistogram = -1;\n  function setHistogramBase(id) {\n    var div = document.getElementById('hist_' + currentHistogram);\n    if (div) {\n      div.style.display = 'none';\n    }\n    div = document.getElementById('hist_' + id);\n    if (div) {\n      div.style.display = '';\n    }\n    var row = document.getElementById('hist_row_' + currentHistogram);\n    if (row) {\n      row.className = '';\n      var button = row.querySelector('button');\n      if (button) button.setAttribute('aria-pressed', 'false');\n    }\n    row = document.getElementById('hist_row_' + id);\n    if (row) {\n      row.className = 'pagespeed-histogram-selected';\n      var button = row.querySelector('button');\n      if (button) button.setAttribute('aria-pressed', 'true');\n    }\n    currentHistogram = id;\n  }\n  function histogramNumber(value) {\n    var number = parseFloat(String(value).replace(/,/g, ''));\n    return isNaN(number) ? 0 : number;\n  }\n  function histogramFormat(value) {\n    var parts = String(value).split('.');\n    parts[0] = parts[0].replace(/\\B(?=(\\d{3})+(?!\\d))/g, ',');\n    return parts.join('.');\n  }\n  function histogramAddMetric(container, label, value) {\n    var card = document.createElement('div');\n    card.className = 'pagespeed-histogram-metric';\n    var title = document.createElement('span');\n    title.className = 'pagespeed-histogram-metric-label';\n    title.textContent = label;\n    var metric = document.createElement('strong');\n    metric.className = 'pagespeed-histogram-metric-value';\n    metric.textContent = value;\n    card.appendChild(title); card.appendChild(metric);\n    container.appendChild(card);\n  }\n  function histogramRows() {\n    var table = document.querySelector('#pagespeed-histograms table.pagespeed-histogram-table');\n    return table ? table.tBodies[0].rows : [];\n  }\n  function exportHistogramCsv() {\n    var table = document.querySelector('#pagespeed-histograms table.pagespeed-histogram-table');\n    if (!table) return;\n    var lines = [];\n    for (var i = 0; i < table.rows.length; i++) {\n      var values = [];\n      for (var j = 0; j < table.rows[i].cells.length; j++) {\n        var value = table.rows[i].cells[j].textContent.replace(/\"/g, '\"\"');\n        values.push('\"' + value + '\"');\n      }\n      lines.push(values.join(','));\n    }\n    var blob = new Blob([lines.join('\\r\\n')], {type: 'text/csv;charset=utf-8'});\n    var url = window.URL.createObjectURL(blob);\n    var link = document.createElement('a'); link.href = url;\n    link.download = 'pagespeed-histograms.csv';\n    document.body.appendChild(link); link.click(); document.body.removeChild(link);\n    window.setTimeout(function() { window.URL.revokeObjectURL(url); }, 1000);\n  }\n  function histogramRange(row) {\n    return row.cells[0].textContent.replace(/\\s+/g, ' ').trim();\n  }\n  function enhanceHistogramDetail(id) {\n    var detail = document.getElementById('hist_' + id);\n    var summaryRow = document.getElementById('hist_row_' + id);\n    if (!detail || !summaryRow) return;\n    if (detail.getAttribute('data-enhanced') === 'true') {\n      var oldHeading = detail.querySelector('.pagespeed-histogram-detail-heading');\n      if (oldHeading) oldHeading.firstChild.textContent = summaryRow.cells[0].textContent;\n      return;\n    }\n    detail.setAttribute('data-enhanced', 'true');\n    detail.className = 'pagespeed-histogram-detail';\n    var heading = document.createElement('h3');\n    heading.className = 'pagespeed-histogram-detail-heading';\n    heading.textContent = summaryRow.cells[0].textContent;\n    var subtitle = document.createElement('small');\n    subtitle.textContent = 'Bucket distribution for this operation';\n    heading.appendChild(subtitle);\n    var grid = document.createElement('div');\n    grid.className = 'pagespeed-histogram-detail-grid';\n    var bucketPanel = document.createElement('section');\n    bucketPanel.className = 'pagespeed-histogram-bucket-panel';\n    var bucketTitle = document.createElement('h4');\n    bucketTitle.className = 'pagespeed-histogram-panel-title';\n    bucketTitle.textContent = 'Bucket distribution';\n    bucketPanel.appendChild(bucketTitle);\n    var bucketWrap = detail.querySelector('.pagespeed-admin-table-wrap');\n    if (bucketWrap) bucketPanel.appendChild(bucketWrap);\n    var side = document.createElement('div');\n    side.className = 'pagespeed-histogram-side';\n    var sharePanel = document.createElement('section');\n    sharePanel.className = 'pagespeed-histogram-share-panel';\n    var shareTitle = document.createElement('h4');\n    shareTitle.className = 'pagespeed-histogram-panel-title';\n    shareTitle.textContent = 'Distribution share';\n    var shareLayout = document.createElement('div');\n    shareLayout.className = 'pagespeed-histogram-share-layout';\n    var donut = document.createElement('div');\n    donut.className = 'pagespeed-histogram-donut';\n    var center = document.createElement('div');\n    center.className = 'pagespeed-histogram-donut-center';\n    var legend = document.createElement('div');\n    legend.className = 'pagespeed-histogram-legend';\n    shareLayout.appendChild(donut); shareLayout.appendChild(legend);\n    sharePanel.appendChild(shareTitle); sharePanel.appendChild(shareLayout);\n    var insightPanel = document.createElement('section');\n    insightPanel.className = 'pagespeed-histogram-insights';\n    var insightTitle = document.createElement('h4');\n    insightTitle.className = 'pagespeed-histogram-panel-title';\n    insightTitle.textContent = 'Key insights';\n    var insightList = document.createElement('div');\n    insightList.className = 'pagespeed-histogram-insight-list';\n    insightPanel.appendChild(insightTitle); insightPanel.appendChild(insightList);\n    side.appendChild(sharePanel); side.appendChild(insightPanel);\n    grid.appendChild(bucketPanel); grid.appendChild(side);\n    detail.insertBefore(heading, detail.firstChild);\n    detail.appendChild(grid);\n    var bucketTable = detail.querySelector('.pagespeed-histogram-buckets');\n    var bucketRows = bucketTable ? bucketTable.tBodies[0].rows : [];\n    var histogramName = summaryRow.cells[0].textContent;\n    if (bucketTable && /latency/i.test(histogramName)) {\n      bucketTable.tHead.rows[0].cells[0].textContent = 'Latency range';\n      bucketTable.tHead.rows[0].cells[1].textContent = 'Requests';\n      var unit = /_ms$/i.test(histogramName) ? ' ms' : (/_us$/i.test(histogramName) ? ' us' : '');\n      if (unit) for (var r = 0; r < bucketRows.length; r++) bucketRows[r].cells[0].textContent += unit;\n    }\n    var colors = ['#39765e', '#6a9c7b', '#d69a45', '#92b7a0', '#b8cbbd'];\n    var stops = []; var cursor = 0; var total = 0; var common = null;\n    for (var i = 0; i < bucketRows.length; i++) {\n      var cells = bucketRows[i].cells;\n      var share = histogramNumber(cells[2].textContent);\n      var count = histogramNumber(cells[1].textContent);\n      var range = histogramRange(bucketRows[i]);\n      if (!common || share > common.share) common = {share: share, range: range};\n      total += count;\n      if (share > 0) {\n        stops.push(colors[i % colors.length] + ' ' + cursor + '% ' + (cursor + share) + '%');\n        cursor += share;\n      }\n      if (i < 4) {\n        var item = document.createElement('div');\n        item.className = 'pagespeed-histogram-legend-item';\n        var swatch = document.createElement('span');\n        swatch.className = 'pagespeed-histogram-legend-swatch';\n        swatch.style.backgroundColor = colors[i % colors.length];\n        var label = document.createElement('span');\n        label.className = 'pagespeed-histogram-legend-range'; label.textContent = range;\n        var percent = document.createElement('strong');\n        percent.textContent = histogramFormat(share.toFixed(1)) + '%';\n        item.appendChild(swatch); item.appendChild(label); item.appendChild(percent);\n        legend.appendChild(item);\n      }\n    }\n    donut.style.background = stops.length ? 'conic-gradient(' + stops.join(',') + ')' : '#e8efea';\n    center.textContent = histogramFormat(String(Math.round(total)));\n    var centerLabel = document.createElement('small'); centerLabel.textContent = 'samples';\n    center.appendChild(centerLabel); donut.appendChild(center);\n    var insights = [];\n    if (common) insights.push(['Most common range', common.range + ' - ' + common.share.toFixed(1) + '% of samples']);\n    insights.push(['95th percentile', summaryRow.cells[8].textContent + ' for this metric']);\n    insights.push(['Spread', 'Std. dev. ' + summaryRow.cells[3].textContent + ' - ' + bucketRows.length + ' populated ranges']);\n    for (var j = 0; j < insights.length; j++) {\n      var insight = document.createElement('div'); insight.className = 'pagespeed-histogram-insight';\n      var insightLabel = document.createElement('strong'); insightLabel.textContent = insights[j][0];\n      insight.appendChild(insightLabel); insight.appendChild(document.createTextNode(insights[j][1]));\n      insightList.appendChild(insight);\n    }\n  }\n  function updateHistogramSummary(id) {\n    var rows = histogramRows(); var visible = 0; var samples = 0;\n    var selected = document.getElementById('hist_row_' + id);\n    for (var i = 0; i < rows.length; i++) {\n      if (rows[i].style.display === 'none') continue;\n      visible++; samples += histogramNumber(rows[i].cells[1].textContent);\n    }\n    var cards = document.querySelectorAll('.pagespeed-histogram-metric-value');\n    if (cards.length >= 4) {\n      cards[0].textContent = histogramFormat(String(visible));\n      cards[1].textContent = histogramFormat(String(Math.round(samples)));\n      cards[2].textContent = selected ? selected.cells[8].textContent : '-';\n      cards[3].textContent = selected ? selected.cells[6].textContent : '-';\n    }\n    enhanceHistogramDetail(id);\n  }\n  function setHistogram(id) { setHistogramBase(id); updateHistogramSummary(id); }\n  (function enhanceHistogramPage() {\n    var root = document.getElementById('pagespeed-histograms');\n    var table = root ? root.querySelector('.pagespeed-histogram-table') : null;\n    if (!root || !table) {\n      var empty = root ? root.querySelector('em') : null;\n      if (empty) empty.className = 'pagespeed-histogram-empty';\n      return;\n    }\n    var rows = table.tBodies[0].rows; var samples = 0;\n    for (var i = 0; i < rows.length; i++) samples += histogramNumber(rows[i].cells[1].textContent);\n    var summary = document.createElement('div'); summary.className = 'pagespeed-histogram-summary';\n    histogramAddMetric(summary, 'Visible histograms', rows.length);\n    histogramAddMetric(summary, 'Metric samples', histogramFormat(String(Math.round(samples))));\n    histogramAddMetric(summary, 'Selected P95', rows.length ? rows[0].cells[8].textContent : '-');\n    histogramAddMetric(summary, 'Selected maximum', rows.length ? rows[0].cells[6].textContent : '-');\n    var toolbar = document.createElement('div'); toolbar.className = 'pagespeed-histogram-toolbar';\n    var copy = document.createElement('div');\n    var title = document.createElement('h2'); title.textContent = 'Histogram details';\n    var description = document.createElement('p'); description.textContent = 'Counts are cumulative since startup. Select a metric to inspect its distribution.';\n    copy.appendChild(title); copy.appendChild(description);\n    var search = document.createElement('input'); search.type = 'search';\n    search.className = 'pagespeed-histogram-search'; search.placeholder = 'Filter histograms...';\n    search.setAttribute('aria-label', 'Filter histograms');\n    var actions = document.createElement('div'); actions.className = 'pagespeed-histogram-actions';\n    var exportButton = document.createElement('button'); exportButton.type = 'button';\n    exportButton.className = 'pagespeed-histogram-export'; exportButton.textContent = 'Export CSV';\n    exportButton.addEventListener('click', exportHistogramCsv);\n    actions.appendChild(search); actions.appendChild(exportButton);\n    toolbar.appendChild(copy); toolbar.appendChild(actions);\n    var tableWrap = table.parentNode; tableWrap.className += ' pagespeed-histogram-table-card';\n    root.insertBefore(summary, tableWrap); root.insertBefore(toolbar, tableWrap);\n    search.addEventListener('input', function() {\n      var term = search.value.toLowerCase(); var first = null;\n      var allRows = table.tBodies[0].rows;\n      for (var j = 0; j < allRows.length; j++) {\n        var match = allRows[j].cells[0].textContent.toLowerCase().indexOf(term) !== -1;\n        allRows[j].style.display = match ? '' : 'none'; if (match && !first) first = allRows[j];\n      }\n      if (first) setHistogram(first.id.replace('hist_row_', ''));\n      else {\n        var activeDetail = document.getElementById('hist_' + currentHistogram);\n        if (activeDetail) activeDetail.style.display = 'none';\n        currentHistogram = -1; updateHistogramSummary(-1);\n      }\n    });\n    setHistogram(0);\n  })();\n</script>\n";

/// Renders one histogram row (`HtmlTableRow`).
fn histogram_table_row(name: &str, index: usize, hist: &Histogram) -> String {
    format!(
        "      <tr id='hist_row_{index}'>\n        <td><button type='button' class='pagespeed-histogram-select'\n                   onclick='setHistogram({index})' aria-pressed='{}'>{name}</button></td>\n        <td>{:.0}</td><td>{:.1}</td><td>{:.1}</td>\n        <td>{:.0}</td><td>{:.0}</td><td>{:.0}</td>\n        <td>{:.0}</td><td>{:.0}</td><td>{:.0}</td>\n     </tr>\n",
        if index == 0 { "true" } else { "false" },
        hist.count(),
        hist.average(),
        hist.standard_deviation(),
        hist.minimum(),
        hist.median(),
        hist.maximum(),
        hist.percentile(90.0),
        hist.percentile(95.0),
        hist.percentile(99.0),
    )
}

/// Renders populated buckets (`WriteRawHistogramData`).
fn histogram_buckets(hist: &Histogram) -> String {
    let mut out = String::from(
        "<div class='pagespeed-admin-table-wrap'><table class='pagespeed-admin-data-table pagespeed-histogram-buckets'><thead><tr><th>Range</th><th>Samples</th><th>Share</th><th>Cumulative</th><th>Distribution</th></tr></thead><tbody>",
    );
    let count = hist.count();
    let mut cumulative = 0.0;
    for index in 0..hist.num_buckets() {
        let value = hist.bucket_count(index);
        if value == 0.0 {
            continue;
        }
        let lower = hist.bucket_start(index);
        let upper = hist.bucket_limit(index);
        let lower_text = if lower.is_infinite() {
            "-&infin;".to_string()
        } else {
            format!("{lower:.0}")
        };
        let upper_text = if upper.is_infinite() {
            "&infin;".to_string()
        } else {
            format!("{upper:.0}")
        };
        let share = if count > 0.0 { value * 100.0 / count } else { 0.0 };
        cumulative += share;
        let distribution = format!(
            "<span class=\"pagespeed-histogram-bar\" style=\"width: {share:.1}%\"></span>"
        );
        out.push_str(&format!(
            "<tr class=\"\"><td class=\"pagespeed-histogram-range\">{lower_text}&ndash;{upper_text}</td><td class=\"pagespeed-histogram-count\">{value:.0}</td><td class=\"pagespeed-histogram-share\">{share:.1}%</td><td class=\"pagespeed-histogram-cumulative\">{cumulative:.1}%</td><td class=\"pagespeed-histogram-distribution\">{distribution}</td></tr>"
        ));
    }
    out.push_str("</tbody></table></div>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    #[test]
    fn variable_basics() {
        let var = Variable::new("v");
        assert_eq!(var.name(), "v");
        assert_eq!(var.get(), 0);
        assert_eq!(var.add(5), 5);
        assert_eq!(var.get(), 5);
        var.clear();
        assert_eq!(var.get(), 0);
    }

    #[test]
    fn up_down_counter_basics() {
        let counter = UpDownCounter::new("u");
        assert_eq!(counter.add(10), 10);
        assert_eq!(counter.add(-4), 6);
        assert_eq!(counter.set_returning_previous(100), 6);
        assert_eq!(counter.get(), 100);
        counter.clear();
        assert_eq!(counter.get(), 0);
    }

    #[test]
    fn counters_thread_safe() {
        let var = Arc::new(Variable::new("v"));
        let up = Arc::new(UpDownCounter::new("u"));
        let mut threads = Vec::new();
        for _ in 0..8 {
            let (var, up) = (var.clone(), up.clone());
            threads.push(std::thread::spawn(move || {
                for _ in 0..1000 {
                    var.add(1);
                    up.add(1);
                    up.add(-1);
                }
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(var.get(), 8000);
        assert_eq!(up.get(), 0);
    }

    #[test]
    fn histogram_layout_and_catchers() {
        let hist = Histogram::new("h");
        assert_eq!(hist.num_buckets(), DEFAULT_NUM_BUCKETS + CATCHER_BUCKETS);
        assert_eq!(hist.num_buckets(), 502);
        assert!(hist.bucket_start(0).is_infinite());
        assert!(hist.bucket_start(0).is_sign_negative());
        assert_eq!(hist.bucket_start(1), 0.0);
        assert_eq!(hist.bucket_start(501), DEFAULT_MAX_VALUE);
        assert!(hist.bucket_start(502).is_infinite());
        assert_eq!(hist.bucket_limit(0), 0.0);
        assert!(hist.is_empty());
        assert_eq!(hist.count(), 0.0);
        assert_eq!(hist.average(), 0.0);
        assert_eq!(hist.maximum(), 0.0); // C++ returns 0 when empty.
        hist.add(-5.0);
        assert_eq!(hist.bucket_count(0), 1.0); // left catcher
        hist.add(6000.0);
        assert_eq!(hist.bucket_count(501), 1.0); // right catcher
        assert_eq!(hist.count(), 2.0);
        assert_eq!(hist.minimum(), -5.0);
        assert_eq!(hist.maximum(), 6000.0);
    }

    #[test]
    fn histogram_statistics() {
        let hist = Histogram::new("h");
        for value in [10.0, 20.0, 30.0, 40.0] {
            hist.add(value);
        }
        assert_eq!(hist.count(), 4.0);
        assert_eq!(hist.average(), 25.0);
        assert_eq!(hist.minimum(), 10.0);
        assert_eq!(hist.maximum(), 40.0);
        // Stddev of {10,20,30,40} is sqrt(125).
        assert!((hist.standard_deviation() - 125f64.sqrt()).abs() < 1e-9);
        // Median lands inside the [20,30) bucket region.
        let median = hist.median();
        assert!((20.0..=30.0).contains(&median), "median={median}");
        // Percentile(0): the empty left catcher immediately satisfies the
        // exact-hit rule, so C++ returns BucketStart(1) == 0.0.
        assert_eq!(hist.percentile(0.0), 0.0);
        assert_eq!(hist.percentile(-1.0), 0.0);
        hist.clear();
        assert!(hist.is_empty());
        assert_eq!(hist.average(), 0.0);
    }

    #[test]
    fn histogram_reconfigure_resets() {
        let hist = Histogram::new("h");
        hist.add(10.0);
        hist.set_max_value(100.0);
        assert!(hist.is_empty());
        assert_eq!(hist.bucket_start(501), 100.0);
        hist.add(10.0);
        hist.set_min_value(5.0);
        assert!(hist.is_empty());
        hist.add(10.0);
        hist.set_suggested_num_buckets(10);
        assert!(hist.is_empty());
        assert_eq!(hist.num_buckets(), 12);
    }

    #[test]
    fn histogram_negative_buckets() {
        let hist = Histogram::new("h");
        hist.enable_negative_buckets();
        hist.add(-100.0);
        hist.add(100.0);
        assert_eq!(hist.count(), 2.0);
        assert_eq!(hist.minimum(), -100.0);
        assert_eq!(hist.maximum(), 100.0);
        // Far out-of-range values still land in catchers.
        hist.add(-6000.0);
        assert_eq!(hist.bucket_count(0), 1.0);
    }

    /// Manually advanced clock for deterministic timed tests.
    #[derive(Debug)]
    struct ManualClock {
        now_ms: AtomicU64,
    }

    impl ManualClock {
        fn new() -> ManualClock {
            ManualClock {
                now_ms: AtomicU64::new(0),
            }
        }

        fn advance(&self, duration: Duration) {
            self.now_ms
                .fetch_add(duration.as_millis() as u64, AtomicOrdering::SeqCst);
        }
    }

    impl Clock for ManualClock {
        fn now(&self) -> Instant {
            // Offset from a fixed base; only differences matter.
            static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
            let base = *BASE.get_or_init(Instant::now);
            base + Duration::from_millis(self.now_ms.load(AtomicOrdering::SeqCst))
        }
    }

    #[test]
    fn timed_variable_windows() {
        let clock = Arc::new(ManualClock::new());
        let var = TimedVariable::with_clock("t", clock.clone());
        var.inc_by(10);
        assert_eq!(var.get(TimedLevel::Start), 10);
        assert_eq!(var.get(TimedLevel::TenSec), 10);
        clock.advance(Duration::from_secs(11));
        assert_eq!(var.get(TimedLevel::TenSec), 0);
        assert_eq!(var.get(TimedLevel::Minute), 10);
        var.inc_by(5);
        assert_eq!(var.get(TimedLevel::TenSec), 5);
        assert_eq!(var.get(TimedLevel::Start), 15);
        clock.advance(Duration::from_secs(3600));
        assert_eq!(var.get(TimedLevel::Hour), 5);
        assert_eq!(var.get(TimedLevel::Start), 15);
        clock.advance(Duration::from_secs(3600));
        assert_eq!(var.get(TimedLevel::Hour), 0);
        assert_eq!(var.get(TimedLevel::Start), 15); // total never pruned
        var.clear();
        assert_eq!(var.get(TimedLevel::Start), 0);
    }

    #[test]
    fn registry_attach_and_snapshot() {
        let stats = Statistics::new();
        let a = stats.add_variable("v");
        let b = stats.add_variable("v");
        a.add(3);
        assert_eq!(b.get(), 3); // same handle (attach, not duplicate)
        assert!(stats.get_variable("missing").is_none());
        stats.add_up_down_counter("u").set(7);
        stats.add_histogram("h").add(1.0);
        stats.add_timed_variable("t", DEFAULT_TIMED_GROUP).inc_by(2);
        let snapshot = stats.snapshot_counters();
        assert_eq!(snapshot.get("v"), Some(&3));
        assert_eq!(snapshot.get("u"), Some(&7));
        // Histograms and timed vars are not plain counters.
        assert!(!snapshot.contains_key("h"));
        assert!(!snapshot.contains_key("t"));
        assert!(stats.get_histogram("h").is_some());
        assert!(stats.get_up_down_counter("u").is_some());
        assert!(stats.get_timed_variable("t").is_some());
    }

    /// Scratch directory for shared counter files.
    struct ShmScratch {
        dir: std::path::PathBuf,
    }

    impl ShmScratch {
        fn open(&self, fixed: &[String]) -> Arc<ps_shm::SharedCounters> {
            Arc::new(ps_shm::SharedCounters::open(&self.dir, fixed).expect("open counters"))
        }
    }

    fn shm_scratch() -> ShmScratch {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("psstats-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        ShmScratch { dir }
    }

    impl Drop for ShmScratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.dir).ok();
        }
    }

    #[test]
    fn shared_fixed_names_aggregate_across_registries() {
        let scratch = shm_scratch();
        let fixed = vec!["hits".to_string(), "gauge".to_string()];
        let first = Statistics::with_shared(scratch.open(&fixed));
        let second = Statistics::with_shared(scratch.open(&fixed));
        first.add_variable("hits").add(4);
        second.add_variable("hits").add(6);
        assert_eq!(10, first.get_variable("hits").unwrap().get());
        assert_eq!(10, second.get_variable("hits").unwrap().get());
        first.add_up_down_counter("gauge").set(3);
        assert_eq!(3, second.add_up_down_counter("gauge").get());
        second.add_up_down_counter("gauge").add(-1);
        assert_eq!(2, first.get_up_down_counter("gauge").unwrap().get());
        assert_eq!(0, first.shared_fallbacks());
        assert_eq!(0, second.shared_fallbacks());
    }

    #[test]
    fn shared_dynamic_names_attach_and_fallbacks_count() {
        let scratch = shm_scratch();
        let stats = Statistics::with_shared(scratch.open(&["fixed".to_string()]));
        // Runtime names attach to spare dynamic slots, shared too.
        let other = Statistics::with_shared(scratch.open(&["fixed".to_string()]));
        stats.add_variable("runtime_seen").add(2);
        assert_eq!(2, other.add_variable("runtime_seen").get());
        assert_eq!(0, stats.shared_fallbacks());
        // Overlong names fall back to local counters (still functional).
        let long = "x".repeat(64);
        stats.add_variable(&long).add(5);
        assert_eq!(5, stats.get_variable(&long).unwrap().get());
        assert_eq!(0, other.get_variable(&long).map_or(0, |var| var.get()));
        assert_eq!(1, stats.shared_fallbacks());
    }

    #[test]
    fn registered_names_lists_both_kinds_sorted() {
        let stats = Statistics::new();
        stats.add_up_down_counter("zebra");
        stats.add_variable("mango");
        stats.add_variable("apple");
        assert_eq!(vec!["apple", "mango", "zebra"], stats.registered_names());
    }

    #[test]
    fn init_all_attaches_with_zero_fallbacks_and_disjoint_kinds() {
        let scratch = shm_scratch();
        // Collect the production fixed set exactly like the factory.
        let probe = Statistics::new();
        crate::names::init_all(&probe);
        let fixed = probe.registered_names();
        assert!(!fixed.is_empty());
        let stats = Statistics::with_shared(scratch.open(&fixed));
        crate::names::init_all(&stats);
        assert_eq!(0, stats.shared_fallbacks());
        // One slot namespace across kinds: no name may register as both.
        let variables = stats.variables.lock().unwrap();
        let up_downs = stats.up_downs.lock().unwrap();
        for name in variables.keys() {
            assert!(
                !up_downs.contains_key(name),
                "counter kind collision: {name}"
            );
        }
    }
}
