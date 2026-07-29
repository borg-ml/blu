use core::{fmt, mem};

const DEFAULT_GC_START_BYTES: usize = 1024 * 1024;
const DEFAULT_GC_GROWTH_PERCENT: u16 = 50;

/// Configuration for VM-owned memory accounting and automatic collection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryConfig {
    /// Maximum accounted bytes. `None` disables the hard limit.
    pub hard_limit_bytes: Option<usize>,
    /// Accounted size at which the first automatic collection is requested.
    pub gc_start_bytes: usize,
    /// Percentage of retained memory added to the next collection threshold.
    pub gc_growth_percent: u16,
    /// Maximum accounted size of one reservation.
    pub max_single_allocation_bytes: usize,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            hard_limit_bytes: None,
            gc_start_bytes: DEFAULT_GC_START_BYTES,
            gc_growth_percent: DEFAULT_GC_GROWTH_PERCENT,
            max_single_allocation_bytes: usize::MAX,
        }
    }
}

/// Snapshot of an account's current memory state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryUsage {
    pub current_bytes: usize,
    pub peak_bytes: usize,
    pub hard_limit_bytes: Option<usize>,
    pub next_gc_bytes: usize,
    pub collections: u64,
}

/// A deterministic memory-accounting or allocation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryError {
    LimitExceeded {
        requested: usize,
        used: usize,
        limit: usize,
    },
    AllocationFailed {
        requested: usize,
    },
    SizeOverflow,
    AccountingUnderflow {
        released: usize,
        used: usize,
    },
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LimitExceeded {
                requested,
                used,
                limit,
            } => write!(
                f,
                "memory reservation of {requested} bytes exceeds limit {limit} with {used} bytes in use"
            ),
            Self::AllocationFailed { requested } => {
                write!(f, "failed to allocate {requested} bytes")
            }
            Self::SizeOverflow => f.write_str("memory accounting size overflow"),
            Self::AccountingUnderflow { released, used } => write!(
                f,
                "cannot release {released} accounted bytes with only {used} bytes in use"
            ),
        }
    }
}

impl std::error::Error for MemoryError {}

/// Returns the conservative charge for a vector with `capacity` elements.
pub const fn checked_vector_bytes<T>(capacity: usize) -> Result<usize, MemoryError> {
    match capacity.checked_mul(mem::size_of::<T>()) {
        Some(bytes) => Ok(bytes),
        None => Err(MemoryError::SizeOverflow),
    }
}

/// Returns a conservative charge for a hash table with usable `capacity`.
///
/// Two raw buckets are charged per usable entry. Each bucket includes aligned
/// key/value storage plus one control byte, avoiding dependence on a specific
/// `HashMap` implementation's load factor and control layout.
pub const fn checked_hash_bytes<K, V>(capacity: usize) -> Result<usize, MemoryError> {
    let entry = mem::size_of::<(K, V)>();
    let alignment = mem::align_of::<(K, V)>();
    let bucket = match entry.checked_add(1) {
        Some(value) => value,
        None => return Err(MemoryError::SizeOverflow),
    };
    let bucket = match checked_align_up(bucket, alignment) {
        Some(value) => value,
        None => return Err(MemoryError::SizeOverflow),
    };
    let raw_buckets = match capacity.checked_mul(2) {
        Some(value) => value,
        None => return Err(MemoryError::SizeOverflow),
    };
    match raw_buckets.checked_mul(bucket) {
        Some(bytes) => Ok(bytes),
        None => Err(MemoryError::SizeOverflow),
    }
}

/// Returns the conservative transient charge while replacing one allocation
/// with another and both buffers may be live.
pub const fn checked_reallocation_peak(
    old_bytes: usize,
    new_bytes: usize,
) -> Result<usize, MemoryError> {
    match old_bytes.checked_add(new_bytes) {
        Some(bytes) => Ok(bytes),
        None => Err(MemoryError::SizeOverflow),
    }
}

const fn checked_align_up(value: usize, alignment: usize) -> Option<usize> {
    debug_assert!(alignment.is_power_of_two());
    let mask = alignment - 1;
    match value.checked_add(mask) {
        Some(value) => Some(value & !mask),
        None => None,
    }
}

/// Tracks deterministic, conservatively charged memory usage.
#[derive(Clone, Debug)]
pub struct MemoryAccount {
    config: MemoryConfig,
    current_bytes: usize,
    peak_bytes: usize,
    next_gc_bytes: usize,
    collections: u64,
}

impl MemoryAccount {
    #[must_use]
    pub fn new(config: MemoryConfig) -> Self {
        let next_gc_bytes = config
            .hard_limit_bytes
            .map_or(config.gc_start_bytes, |limit| {
                config.gc_start_bytes.min(limit)
            });
        Self {
            config,
            current_bytes: 0,
            peak_bytes: 0,
            next_gc_bytes,
            collections: 0,
        }
    }

    #[must_use]
    pub const fn usage(&self) -> MemoryUsage {
        MemoryUsage {
            current_bytes: self.current_bytes,
            peak_bytes: self.peak_bytes,
            hard_limit_bytes: self.config.hard_limit_bytes,
            next_gc_bytes: self.next_gc_bytes,
            collections: self.collections,
        }
    }

    #[must_use]
    pub const fn should_collect(&self, requested: usize) -> bool {
        match self.current_bytes.checked_add(requested) {
            Some(required) => required > self.next_gc_bytes,
            None => true,
        }
    }

    pub fn reserve(&mut self, bytes: usize) -> Result<MemoryReservation<'_>, MemoryError> {
        if bytes > self.config.max_single_allocation_bytes {
            return Err(MemoryError::LimitExceeded {
                requested: bytes,
                used: self.current_bytes,
                limit: self.config.max_single_allocation_bytes,
            });
        }
        let required = self
            .current_bytes
            .checked_add(bytes)
            .ok_or(MemoryError::SizeOverflow)?;
        if let Some(limit) = self.config.hard_limit_bytes
            && required > limit
        {
            return Err(MemoryError::LimitExceeded {
                requested: bytes,
                used: self.current_bytes,
                limit,
            });
        }
        self.current_bytes = required;
        self.peak_bytes = self.peak_bytes.max(required);
        Ok(MemoryReservation {
            account: self,
            bytes,
            committed: false,
        })
    }

    pub fn release(&mut self, bytes: usize) -> Result<(), MemoryError> {
        self.current_bytes =
            self.current_bytes
                .checked_sub(bytes)
                .ok_or(MemoryError::AccountingUnderflow {
                    released: bytes,
                    used: self.current_bytes,
                })?;
        Ok(())
    }

    pub fn finish_collection(&mut self) {
        self.collections = self.collections.saturating_add(1);
        let proportional_growth = self
            .current_bytes
            .saturating_mul(usize::from(self.config.gc_growth_percent))
            / 100;
        let growth = proportional_growth.max(self.config.gc_start_bytes);
        let threshold = self.current_bytes.saturating_add(growth);
        self.next_gc_bytes = self
            .config
            .hard_limit_bytes
            .map_or(threshold, |limit| threshold.min(limit));
    }
}

impl Default for MemoryAccount {
    fn default() -> Self {
        Self::new(MemoryConfig::default())
    }
}

/// A provisional charge that rolls back unless explicitly committed.
#[derive(Debug)]
pub struct MemoryReservation<'a> {
    account: &'a mut MemoryAccount,
    bytes: usize,
    committed: bool,
}

impl MemoryReservation<'_> {
    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn commit(mut self) {
        self.committed = true;
    }

    /// Commits this reservation while releasing an allocation it replaces.
    pub fn commit_replacing(mut self, replaced_bytes: usize) -> Result<(), MemoryError> {
        let used_before_reservation = self.account.current_bytes.checked_sub(self.bytes).ok_or(
            MemoryError::AccountingUnderflow {
                released: self.bytes,
                used: self.account.current_bytes,
            },
        )?;
        if replaced_bytes > used_before_reservation {
            return Err(MemoryError::AccountingUnderflow {
                released: replaced_bytes,
                used: used_before_reservation,
            });
        }
        self.account.current_bytes -= replaced_bytes;
        self.committed = true;
        Ok(())
    }
}

impl Drop for MemoryReservation<'_> {
    fn drop(&mut self) {
        if !self.committed {
            debug_assert!(self.account.current_bytes >= self.bytes);
            self.account.current_bytes -= self.bytes;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_configuration_is_unlimited_and_deterministic() {
        assert_eq!(
            MemoryConfig::default(),
            MemoryConfig {
                hard_limit_bytes: None,
                gc_start_bytes: 1024 * 1024,
                gc_growth_percent: 50,
                max_single_allocation_bytes: usize::MAX,
            }
        );
    }

    #[test]
    fn checked_capacity_formulas_charge_conservatively() {
        assert_eq!(
            checked_vector_bytes::<u64>(4),
            Ok(4 * mem::size_of::<u64>())
        );
        let bucket = checked_align_up(
            mem::size_of::<(u32, u64)>() + 1,
            mem::align_of::<(u32, u64)>(),
        )
        .unwrap();
        assert_eq!(checked_hash_bytes::<u32, u64>(4), Ok(8 * bucket));
        assert_eq!(checked_reallocation_peak(80, 160), Ok(240));
    }

    #[test]
    fn checked_capacity_formulas_report_overflow() {
        assert_eq!(
            checked_vector_bytes::<u64>(usize::MAX),
            Err(MemoryError::SizeOverflow)
        );
        assert_eq!(
            checked_hash_bytes::<u64, u64>(usize::MAX),
            Err(MemoryError::SizeOverflow)
        );
        assert_eq!(
            checked_reallocation_peak(usize::MAX, 1),
            Err(MemoryError::SizeOverflow)
        );
    }

    #[test]
    fn reservation_rolls_back_unless_committed() {
        let mut account = MemoryAccount::default();
        {
            let reservation = account.reserve(64).unwrap();
            assert_eq!(reservation.bytes(), 64);
        }
        assert_eq!(account.usage().current_bytes, 0);
        assert_eq!(account.usage().peak_bytes, 64);

        account.reserve(32).unwrap().commit();
        assert_eq!(account.usage().current_bytes, 32);
        account.release(32).unwrap();
        assert_eq!(account.usage().current_bytes, 0);
    }

    #[test]
    fn replacement_commit_atomically_swaps_charges() {
        let mut account = MemoryAccount::default();
        account.reserve(40).unwrap().commit();
        account.reserve(64).unwrap().commit_replacing(40).unwrap();
        assert_eq!(account.usage().current_bytes, 64);
        assert_eq!(account.usage().peak_bytes, 104);

        let error = account.reserve(8).unwrap().commit_replacing(65);
        assert_eq!(
            error,
            Err(MemoryError::AccountingUnderflow {
                released: 65,
                used: 64,
            })
        );
        assert_eq!(account.usage().current_bytes, 64);
        assert_eq!(account.usage().peak_bytes, 104);
    }

    #[test]
    fn account_enforces_single_and_hard_limits_without_mutation() {
        let mut account = MemoryAccount::new(MemoryConfig {
            hard_limit_bytes: Some(100),
            gc_start_bytes: 40,
            gc_growth_percent: 50,
            max_single_allocation_bytes: 80,
        });
        account.reserve(60).unwrap().commit();
        assert_eq!(
            account.reserve(81).unwrap_err(),
            MemoryError::LimitExceeded {
                requested: 81,
                used: 60,
                limit: 80,
            }
        );
        assert_eq!(
            account.reserve(41).unwrap_err(),
            MemoryError::LimitExceeded {
                requested: 41,
                used: 60,
                limit: 100,
            }
        );
        assert_eq!(account.usage().current_bytes, 60);
    }

    #[test]
    fn account_reports_underflow() {
        let mut account = MemoryAccount::default();
        assert_eq!(
            account.release(1),
            Err(MemoryError::AccountingUnderflow {
                released: 1,
                used: 0,
            })
        );
    }

    #[test]
    fn collection_threshold_uses_retained_bytes_and_growth_policy() {
        let mut account = MemoryAccount::new(MemoryConfig {
            hard_limit_bytes: Some(1_000),
            gc_start_bytes: 100,
            gc_growth_percent: 50,
            max_single_allocation_bytes: usize::MAX,
        });
        assert!(!account.should_collect(100));
        assert!(account.should_collect(101));
        account.reserve(400).unwrap().commit();
        account.finish_collection();
        assert_eq!(
            account.usage(),
            MemoryUsage {
                current_bytes: 400,
                peak_bytes: 400,
                hard_limit_bytes: Some(1_000),
                next_gc_bytes: 600,
                collections: 1,
            }
        );
    }
}
