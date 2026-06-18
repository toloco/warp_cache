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
        // Release fence so the odd-seq publish is ordered BEFORE the data
        // mutations that follow (#40). A Release store alone orders only the ops
        // *preceding* it, so without this fence the data writes can float ahead of
        // the odd store on weak-memory hardware (ARM64) — a reader could then see
        // mutated data while seq still reads even at both read_begin and
        // read_validate, falsely validating a torn read. This is the textbook
        // seqlock writer-enter construction (the exit-side Release at write_unlock
        // orders data writes before going even, but cannot cover the entry side).
        std::sync::atomic::fence(Ordering::Release);
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

/// Loom model of the seqlock's reader/writer memory ordering (issue #40).
///
/// The real `ShmSeqLock` reads/writes atomics through raw mmap pointers, which loom
/// cannot track, so the *algorithm* is replicated here with loom atomics. The orderings
/// mirror the real code exactly: writer goes odd (Release store) + Release fence, mutates
/// data, goes even (Release store); reader loads seq (Acquire), reads data, Acquire fence,
/// re-loads seq, and only trusts the data if the seq is unchanged and even.
///
/// Run with: `RUSTFLAGS="--cfg loom" cargo test --lib seqlock_ordering`
///
/// Deleting the `fence(Release)` below makes loom find an execution where a reader
/// validates a torn read (one data word from the old write, one from the new) — the #40
/// bug. With the fence, no such execution exists.
#[cfg(loom)]
mod loom_tests {
    use loom::sync::atomic::{fence, AtomicU64, Ordering};
    use loom::sync::Arc;
    use loom::thread;

    #[test]
    fn seqlock_ordering_no_torn_validated_read() {
        loom::model(|| {
            // `d0`/`d1` are two data words the writer always keeps equal; if a reader
            // ever validates a read with d0 != d1, it observed a torn write.
            let seq = Arc::new(AtomicU64::new(0));
            let d0 = Arc::new(AtomicU64::new(0));
            let d1 = Arc::new(AtomicU64::new(0));

            let writer = {
                let (seq, d0, d1) = (seq.clone(), d0.clone(), d1.clone());
                thread::spawn(move || {
                    // write_lock: go odd (spinlock omitted — single writer).
                    let prev = seq.load(Ordering::Relaxed);
                    seq.store(prev + 1, Ordering::Release);
                    fence(Ordering::Release); // #40 fix — delete to see loom fail
                                              // data mutation, kept internally consistent
                    d0.store(1, Ordering::Relaxed);
                    d1.store(1, Ordering::Relaxed);
                    // write_unlock: go even
                    let prev = seq.load(Ordering::Relaxed);
                    seq.store(prev + 1, Ordering::Release);
                })
            };

            // reader: one optimistic pass (read_begin + read_validate).
            let s1 = seq.load(Ordering::Acquire);
            let v0 = d0.load(Ordering::Relaxed);
            let v1 = d1.load(Ordering::Relaxed);
            fence(Ordering::Acquire);
            let s2 = seq.load(Ordering::Relaxed);
            if s1 & 1 == 0 && s1 == s2 {
                assert_eq!(v0, v1, "torn read validated as consistent (#40)");
            }

            writer.join().unwrap();
        });
    }
}
