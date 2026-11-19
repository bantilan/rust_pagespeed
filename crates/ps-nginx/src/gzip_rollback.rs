//! The record of the gzip configuration the setup wrote, so
//! it can undo it when the user configured gzip explicitly
//! (`NgxGZipRollbackLog`).
//!
//! The setup writes into one configuration cycle's memory and
//! rolls back out of it, so the record is per-cycle:
//! `begin_cycle` starts it empty, the `record_*` calls log
//! every slot the setup writes, and `roll_back` resets every
//! logged slot through nginx callbacks and empties the record.
//! A cycle whose setup rolled back stays disabled; a new cycle
//! re-arms the setup when the gzip module is available in it.
//! Without the per-cycle reset, slots recorded during one
//! configuration would be written again (through freed memory)
//! when a later reload rolls back.

/// Per-cycle record of written gzip slots (`NgxGZipRollbackLog`).
///
/// Slots are raw pointers into the configuration-cycle pool,
/// exactly like C++; the pool outlives the record within a
/// cycle, and `begin_cycle` drops every slot when the pool is
/// replaced.
#[derive(Debug, Default)]
pub struct RollbackLog {
    flags: Vec<*mut isize>,
    uints: Vec<*mut usize>,
    types: Vec<*mut *mut ()>,
    enabled: bool,
}

impl RollbackLog {
    /// Creates an empty, disabled record.
    #[must_use]
    pub fn new() -> RollbackLog {
        RollbackLog::default()
    }

    /// Starts the record for a configuration cycle, dropping
    /// whatever an earlier cycle recorded (`BeginCycle`). The
    /// automatic setup is armed when the gzip module is
    /// available in the new cycle.
    pub fn begin_cycle(&mut self, gzip_available: bool) {
        // Slots recorded earlier point into a configuration that is being
        // replaced (and is freed once the new one is in place).
        self.flags.clear();
        self.uints.clear();
        self.types.clear();
        self.enabled = gzip_available;
    }

    /// Whether the automatic setup may write in this cycle:
    /// available, and not rolled back after an explicit gzip
    /// directive.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Logs a flag slot the setup wrote (`RecordFlag`).
    pub fn record_flag(&mut self, slot: *mut isize) {
        self.flags.push(slot);
    }

    /// Logs an unsigned slot the setup wrote (`RecordUint`).
    pub fn record_uint(&mut self, slot: *mut usize) {
        self.uints.push(slot);
    }

    /// Logs a types slot the setup wrote (`RecordTypes`).
    pub fn record_types(&mut self, slot: *mut *mut ()) {
        self.types.push(slot);
    }

    /// How many slots are recorded. Tests only.
    #[must_use]
    pub fn recorded(&self) -> usize {
        self.flags.len() + self.uints.len() + self.types.len()
    }

    /// Resets every recorded slot through the given callbacks
    /// and empties the record (`RollBack`). Disables the
    /// automatic setup for the rest of the cycle.
    pub fn roll_back(
        &mut self,
        mut reset_flag: impl FnMut(*mut isize),
        mut reset_uint: impl FnMut(*mut usize),
        mut reset_types: impl FnMut(*mut *mut ()),
    ) {
        for index in 0..self.flags.len() {
            reset_flag(self.flags[index]);
        }
        for index in 0..self.uints.len() {
            reset_uint(self.uints[index]);
        }
        for index in 0..self.types.len() {
            reset_types(self.types[index]);
        }
        self.flags.clear();
        self.uints.clear();
        self.types.clear();
        self.enabled = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNSET_FLAG: isize = -1;
    const UNSET_UINT: usize = usize::MAX;

    /// A configuration cycle's pool: the slots the setup writes
    /// into. Freed when the cycle is replaced, like nginx's
    /// configuration pool.
    #[derive(Debug, Default)]
    struct CyclePool {
        gzip: isize,
        vary: isize,
        http_version: usize,
        types: *mut (),
    }

    // Rolls back the way the setter does: writes the unset value
    // through every recorded slot.
    fn roll_back_all(log: &mut RollbackLog) -> Vec<*mut ()> {
        let touched = std::cell::RefCell::new(Vec::new());
        log.roll_back(
            // SAFETY: test-only simulation of the nginx pool write;
            // each slot points at a live `CyclePool` field.
            |slot| unsafe {
                *slot = UNSET_FLAG;
                touched.borrow_mut().push(slot.cast::<()>());
            },
            // SAFETY: as above.
            |slot| unsafe {
                *slot = UNSET_UINT;
                touched.borrow_mut().push(slot.cast::<()>());
            },
            // SAFETY: as above.
            |slot| unsafe {
                *slot = std::ptr::null_mut();
                touched.borrow_mut().push(slot.cast::<()>());
            },
        );
        touched.into_inner()
    }

    fn set_up_location(log: &mut RollbackLog, pool: &mut CyclePool) {
        pool.gzip = 1;
        log.record_flag(std::ptr::addr_of_mut!(pool.gzip));
        pool.vary = 1;
        log.record_flag(std::ptr::addr_of_mut!(pool.vary));
        pool.http_version = 10;
        log.record_uint(std::ptr::addr_of_mut!(pool.http_version));
        log.record_types(std::ptr::addr_of_mut!(pool.types));
    }

    #[test]
    fn roll_back_resets_this_cycles_slots() {
        let mut log = RollbackLog::new();
        log.begin_cycle(true);
        assert!(log.enabled());
        let mut pool = CyclePool::default();
        set_up_location(&mut log, &mut pool);
        assert_eq!(4, roll_back_all(&mut log).len());
        assert_eq!(UNSET_FLAG, pool.gzip);
        assert_eq!(UNSET_FLAG, pool.vary);
        assert_eq!(UNSET_UINT, pool.http_version);
        assert!(!log.enabled());
    }

    #[test]
    fn second_cycle_never_touches_first_cycles_slots() {
        let mut log = RollbackLog::new();
        log.begin_cycle(true);
        let mut first = Box::new(CyclePool::default());
        set_up_location(&mut log, &mut first);
        let first_address = &*first as *const CyclePool as *const u8;
        // Reload: the first cycle's pool goes away and a new cycle starts.
        drop(first);

        log.begin_cycle(true);
        assert_eq!(0, log.recorded());
        let mut second = CyclePool::default();
        set_up_location(&mut log, &mut second);
        let touched = roll_back_all(&mut log);
        assert_eq!(4, touched.len());
        let base = std::ptr::addr_of!(second) as usize;
        let end = base + std::mem::size_of::<CyclePool>();
        for slot in &touched {
            let address = *slot as usize;
            assert!(address >= base && address < end);
            assert_ne!(first_address as usize, address);
        }
        assert_eq!(UNSET_FLAG, second.gzip);
    }

    #[test]
    fn roll_back_in_one_cycle_does_not_disable_the_next() {
        let mut log = RollbackLog::new();
        log.begin_cycle(true);
        let mut first = CyclePool::default();
        set_up_location(&mut log, &mut first);
        roll_back_all(&mut log);
        assert!(!log.enabled());

        // The next configuration may not carry an explicit gzip directive:
        // the automatic setup is armed again for it.
        log.begin_cycle(true);
        assert!(log.enabled());
    }

    #[test]
    fn unavailable_gzip_stays_disabled() {
        let mut log = RollbackLog::new();
        log.begin_cycle(false);
        assert!(!log.enabled());
        log.begin_cycle(true);
        assert!(log.enabled());
        log.begin_cycle(false);
        assert!(!log.enabled());
    }
}
