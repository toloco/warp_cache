/// Seqlock for shared memory: optimistic lock-free reads + TTAS spinlock for writers.
///
/// Layout in shared memory (64 bytes, one cache line):
///   [seq_counter: u64][write_lock: u32][padding to 64]
///
/// Readers check seq before/after reading — no kernel calls, ~10-20ns.
/// Writers acquire a TTAS spinlock then bump seq odd→even.
use std::io;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Size reserved for the lock in the mmap region — one cache line.
pub const LOCK_SIZE: usize = 64;

/// A seqlock stored in shared memory for cross-process use.
pub struct ShmSeqLock {
    seq_ptr: *const AtomicU64,
    write_lock_ptr: *const AtomicU32,
}

unsafe impl Send for ShmSeqLock {}
unsafe impl Sync for ShmSeqLock {}

impl ShmSeqLock {
    /// Initialize a new seqlock at the given memory location.
    ///
    /// # Safety
    /// `ptr` must point to at least `LOCK_SIZE` bytes of shared memory, zeroed.
    pub unsafe fn init(ptr: *mut u8) -> io::Result<Self> {
        // Zero the region (caller should have done this, but be safe)
        std::ptr::write_bytes(ptr, 0, LOCK_SIZE);

        let seq_ptr = ptr as *const AtomicU64;
        let write_lock_ptr = ptr.add(8) as *const AtomicU32;

        // Explicitly store initial values
        (*seq_ptr).store(0, Ordering::Relaxed);
        (*write_lock_ptr).store(0, Ordering::Relaxed);

        Ok(ShmSeqLock {
            seq_ptr,
            write_lock_ptr,
        })
    }

    /// Attach to an already-initialized seqlock at the given memory location.
    ///
    /// # Safety
    /// `ptr` must point to a previously initialized seqlock in shared memory.
    pub unsafe fn from_existing(ptr: *mut u8) -> Self {
        ShmSeqLock {
            seq_ptr: ptr as *const AtomicU64,
            write_lock_ptr: ptr.add(8) as *const AtomicU32,
        }
    }

    /// Begin an optimistic read. Returns the sequence number.
    /// Spins until the sequence is even (no writer active).
    #[inline]
    pub fn read_begin(&self) -> u64 {
        loop {
            let seq = unsafe { &*self.seq_ptr }.load(Ordering::Acquire);
            if seq & 1 == 0 {
                return seq;
            }
            std::hint::spin_loop();
        }
    }

    /// Validate that no writer modified data since `read_begin()` returned `seq`.
    /// Returns true if the read was consistent (safe to use the data).
    #[inline]
    pub fn read_validate(&self, seq: u64) -> bool {
        // Ensure all data reads complete before we re-check seq.
        // This is critical on ARM64 where loads can be reordered.
        std::sync::atomic::fence(Ordering::Acquire);
        let current = unsafe { &*self.seq_ptr }.load(Ordering::Relaxed);
        current == seq
    }

    /// Acquire the write lock. Blocks (spins) until acquired.
    #[inline]
    pub fn write_lock(&self) {
        let lock = unsafe { &*self.write_lock_ptr };
        // TTAS (Test-and-Test-and-Set) spinlock
        loop {
            // Test: spin on load (cache-friendly, no bus traffic)
            while lock.load(Ordering::Relaxed) != 0 {
                std::hint::spin_loop();
            }
            // Test-and-Set: try to acquire
            if lock
                .compare_exchange_weak(0, 1, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
        }
        // Bump seq to odd — signals "writer active"
        let seq = unsafe { &*self.seq_ptr };
        let prev = seq.load(Ordering::Relaxed);
        seq.store(prev + 1, Ordering::Release);
    }

    /// Release the write lock.
    #[inline]
    pub fn write_unlock(&self) {
        // Bump seq to even — all data mutations now visible to readers
        let seq = unsafe { &*self.seq_ptr };
        let prev = seq.load(Ordering::Relaxed);
        seq.store(prev + 1, Ordering::Release);

        // Release the spinlock
        unsafe { &*self.write_lock_ptr }.store(0, Ordering::Release);
    }
}
