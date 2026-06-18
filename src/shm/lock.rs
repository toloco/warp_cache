/// Seqlock for shared memory: optimistic lock-free reads + a robust writer spinlock.
///
/// Layout in shared memory (64 bytes, one cache line):
///   [seq_counter: u64][owner_pid: u32][padding to 64]
///
/// Readers check seq before/after reading — no kernel calls, ~10-20ns.
/// Writers acquire the spinlock (stamping their PID as the owner) then bump seq
/// odd→even.
///
/// ## Death-handling (#38)
/// The writer slot stores the owner's PID rather than a bare 0/1 flag. If a
/// process is killed (SIGKILL/OOM/crash) while holding the lock, every other
/// process would otherwise spin forever — `write_lock()` waiting for the flag to
/// clear and `read_begin()` waiting for `seq` to go even. To recover, a waiter
/// that has spun past `RECOVER_SPINS` probes the owner with `kill(pid, 0)`; if
/// the owner is gone (`ESRCH`) it stamps *its own* PID on the lock, restores `seq`
/// parity, then releases — making the structure usable again. Stamping the
/// recoverer's PID (rather than a fixed sentinel) keeps recovery itself
/// crash-safe: if the recoverer is killed mid-recovery the lock holds *its* now
/// dead PID, which the same `kill(pid, 0)` path recovers — so no terminal "stuck"
/// owner value can wedge the cache forever.
///
/// This restores *liveness*, not the consistency of whatever the dead writer was
/// mutating: a process killed mid-`insert` can leave a half-written entry or an
/// inconsistent free-list/size. That is unavoidable without a write-ahead log and
/// is the accepted trade-off — a recoverable cache beats a permanent global
/// deadlock. Panics (the common in-process failure, e.g. a serde or pointer-math
/// bug) are handled by `WriteGuard`'s `Drop`, which releases the lock and restores
/// parity during unwinding, so a panic across the PyO3 boundary can't wedge the
/// cache either.
///
/// PID-reuse note: recovery keys off `kill(pid, 0)`. If a dead owner's PID is
/// reused by a *live*, unrelated process before a waiter probes it, the probe
/// reports "alive" and waiters keep spinning (slow, never incorrect) until that
/// PID also exits. Sharing the mmap across PID namespaces (a container seeing
/// different PIDs for the same region) is unsupported for the same reason.
use std::io;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Size reserved for the lock in the mmap region — one cache line.
pub const LOCK_SIZE: usize = 64;

/// Spin iterations a waiter tolerates on a *continuously held* lock before it
/// probes the owner for death. Far longer than any real critical section
/// (pointer math + a bounded memcpy, sub-microsecond), so a live holder is never
/// disturbed; only a dead/wedged owner reaches it. The probe is one `kill(2)`
/// syscall, after which a live owner just resets the counter and keeps spinning.
const RECOVER_SPINS: u32 = 1 << 16;

/// This process's PID, used as the owner stamp stored in the lock word.
#[inline]
fn current_pid() -> u32 {
    // SAFETY: getpid() always succeeds, is async-signal-safe, and returns a
    // positive pid_t that fits in u32.
    (unsafe { libc::getpid() }) as u32
}

/// True only if `pid` is a real, individually-addressable PID that no longer
/// exists (`kill(pid,0)==ESRCH`). The lock word only ever holds `0` (free) or a
/// live/recovering process's PID, so both `current_pid()` and recovery store
/// values in `1..=i32::MAX`. We still reject `0` and any value `> i32::MAX`
/// defensively: `as i32` would make those negative or zero, and `kill` with a
/// non-positive argument targets process *groups* / every signalable process —
/// never something a liveness probe may do. A live owner (including `EPERM`,
/// "exists but unsignalable") returns false, so recovery never steals from a
/// running process.
#[inline]
fn owner_is_dead(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    // kill(pid, 0) sends no signal; it only checks deliverability.
    let rc = unsafe { libc::kill(pid as i32, 0) };
    rc != 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

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

        // Explicitly store initial values (0 owner = unlocked).
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
        let mut spins: u32 = 0;
        loop {
            let seq = unsafe { &*self.seq_ptr }.load(Ordering::Acquire);
            if seq & 1 == 0 {
                return seq;
            }
            // A writer is active. If it died without going even we'd spin here
            // forever, so periodically probe the owner and recover a dead one (#38).
            spins = spins.wrapping_add(1);
            if spins >= RECOVER_SPINS {
                spins = 0;
                let owner = unsafe { &*self.write_lock_ptr }.load(Ordering::Relaxed);
                if owner_is_dead(owner) {
                    self.recover_from_dead(owner);
                }
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

    /// Acquire the write lock. Blocks (spins) until acquired, then returns a
    /// `WriteGuard` that releases it on drop (including during panic unwinding).
    #[inline]
    pub fn write_lock(&self) -> WriteGuard<'_> {
        let lock = unsafe { &*self.write_lock_ptr };
        let me = current_pid();
        let mut spins: u32 = 0;
        // TTAS (Test-and-Test-and-Set) spinlock, stamping our PID as the owner so
        // a dead holder can be detected and recovered (#38).
        loop {
            // Test: spin on load (cache-friendly, no bus traffic).
            let owner = lock.load(Ordering::Relaxed);
            if owner == 0 {
                // Test-and-Set: try to acquire by stamping our PID.
                if lock
                    .compare_exchange_weak(0, me, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    break;
                }
                // Lost the race to another writer; reset the death timer.
                spins = 0;
            } else {
                // Held by `owner`. If it stays held far too long, the owner may
                // have died — probe it and recover if so.
                spins = spins.wrapping_add(1);
                if spins >= RECOVER_SPINS {
                    spins = 0;
                    if owner_is_dead(owner) {
                        self.recover_from_dead(owner);
                    }
                }
            }
            std::hint::spin_loop();
        }
        // Enter the critical section: ensure seq is odd ("writer active").
        // Normally seq is even here (the previous writer left it even); the check
        // only matters on the recovery path, which may hand us an already-odd seq.
        let seq = unsafe { &*self.seq_ptr };
        let s = seq.load(Ordering::Relaxed);
        if s & 1 == 0 {
            seq.store(s + 1, Ordering::Release);
        }
        // Release fence so the odd-seq publish is ordered BEFORE the data
        // mutations that follow (#40). A Release store alone orders only the ops
        // *preceding* it, so without this fence the data writes can float ahead of
        // the odd store on weak-memory hardware (ARM64) — a reader could then see
        // mutated data while seq still reads even at both read_begin and
        // read_validate, falsely validating a torn read. This is the textbook
        // seqlock writer-enter construction (the exit-side Release at write_unlock
        // orders data writes before going even, but cannot cover the entry side).
        std::sync::atomic::fence(Ordering::Release);
        WriteGuard { lock: self }
    }

    /// Release the write lock and publish all data mutations. Invoked by
    /// `WriteGuard::drop` — including during panic unwinding — so a panic in the
    /// critical section can never leak the lock or leave `seq` odd (#38).
    fn write_unlock(&self) {
        // Bump seq to even — all data mutations now visible to readers.
        let seq = unsafe { &*self.seq_ptr };
        let s = seq.load(Ordering::Relaxed);
        seq.store(s + 1, Ordering::Release);

        // Release the spinlock (clear the owner).
        unsafe { &*self.write_lock_ptr }.store(0, Ordering::Release);
    }

    /// Recover a lock whose owner (`dead_pid`) is confirmed dead: force `seq` even
    /// and free the lock so readers and the next writer can proceed (#38).
    ///
    /// Idempotent under concurrent detection — only the waiter whose CAS swaps the
    /// dead PID out performs the fix-up; the rest fall back to their retry loop.
    /// We swap in *our own* PID (not a fixed sentinel): it keeps the lock held (a
    /// non-zero, live owner) while we restore `seq`, so no writer can acquire and
    /// enter against a stale odd `seq` — and it keeps recovery itself crash-safe.
    /// If we are killed between the CAS and the final release, the lock is left
    /// holding our now-dead PID, which the ordinary `owner_is_dead` path recovers;
    /// a fixed sentinel would instead be a terminal, un-probeable wedge. Restores
    /// liveness only — see the module docs on in-flight data a dead writer left torn.
    fn recover_from_dead(&self, dead_pid: u32) {
        // dead_pid was confirmed dead by the caller, so it can't equal our (live)
        // PID; the CAS below therefore always makes real progress when it wins.
        if dead_pid == 0 {
            return;
        }
        let lock = unsafe { &*self.write_lock_ptr };
        if lock
            .compare_exchange(dead_pid, current_pid(), Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            let seq = unsafe { &*self.seq_ptr };
            let s = seq.load(Ordering::Relaxed);
            if s & 1 != 0 {
                seq.store(s + 1, Ordering::Release); // odd → even
            }
            lock.store(0, Ordering::Release); // fully release
        }
    }
}

/// RAII release for the write lock. Dropping it (normally, or while unwinding a
/// panic) bumps `seq` even and clears the owner, so the critical section is always
/// closed even if the body panics (#38).
#[must_use = "the write lock is released as soon as the guard is dropped"]
pub struct WriteGuard<'a> {
    lock: &'a ShmSeqLock,
}

impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        self.lock.write_unlock();
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

/// Death-handling tests (#38): a process killed while holding the write lock must
/// not wedge other processes forever, and a panic in the critical section must
/// still release the lock.
#[cfg(all(test, not(loom)))]
mod recovery_tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::time::Duration;

    /// A PID guaranteed not to be running: fork a child that exits immediately and
    /// reap it. (Reuse within the test window is astronomically unlikely.)
    fn dead_pid() -> u32 {
        // SAFETY: the child does nothing but _exit, so the classic post-fork
        // async-signal-safety hazards don't apply.
        unsafe {
            let pid = libc::fork();
            assert!(pid >= 0, "fork failed");
            if pid == 0 {
                libc::_exit(0);
            }
            let mut status = 0;
            libc::waitpid(pid, &mut status, 0);
            pid as u32
        }
    }

    /// Build a seqlock backed by a leaked 'static buffer and return its address,
    /// so it can be rebuilt inside spawned threads (raw pointers aren't Send).
    fn leaked_lock_addr() -> usize {
        let buf = Box::leak(Box::new([0u8; LOCK_SIZE]));
        let addr = buf.as_mut_ptr() as usize;
        unsafe { ShmSeqLock::init(addr as *mut u8).unwrap() };
        addr
    }

    fn lock_at(addr: usize) -> ShmSeqLock {
        unsafe { ShmSeqLock::from_existing(addr as *mut u8) }
    }

    /// Run `f` on a thread; fail (rather than hang the suite) if it does not finish
    /// in `timeout` — that signals the lock is wedged and death-handling regressed.
    fn run_or_fail(label: &str, timeout: Duration, f: impl FnOnce() + Send + 'static) {
        let done = Arc::new(AtomicBool::new(false));
        let d2 = done.clone();
        let h = std::thread::spawn(move || {
            f();
            d2.store(true, Ordering::SeqCst);
        });
        let step = Duration::from_millis(10);
        let mut waited = Duration::ZERO;
        while waited < timeout {
            if done.load(Ordering::SeqCst) {
                h.join().unwrap();
                return;
            }
            std::thread::sleep(step);
            waited += step;
        }
        panic!("{label} did not complete in {timeout:?} — lock is wedged (#38 regressed)");
    }

    /// Simulate a writer that went odd then was SIGKILLed before unlocking.
    fn wedge_with_dead_owner(addr: usize, dead: u32) {
        let l = lock_at(addr);
        unsafe {
            (*l.seq_ptr).store(1, Ordering::Release); // odd: "writer active"
            (*l.write_lock_ptr).store(dead, Ordering::Release); // held by a dead pid
        }
    }

    #[test]
    fn write_lock_recovers_from_dead_owner() {
        let addr = leaked_lock_addr();
        wedge_with_dead_owner(addr, dead_pid());

        // Before the fix this spins forever; with recovery it steals and returns.
        run_or_fail("write_lock", Duration::from_secs(5), move || {
            let lock = lock_at(addr);
            let _g = lock.write_lock();
        });

        let l = lock_at(addr);
        assert_eq!(
            unsafe { &*l.write_lock_ptr }.load(Ordering::Relaxed),
            0,
            "lock not released after a recovered write"
        );
        assert_eq!(
            unsafe { &*l.seq_ptr }.load(Ordering::Relaxed) & 1,
            0,
            "seq left odd after a recovered write"
        );
    }

    #[test]
    fn read_begin_recovers_from_dead_owner() {
        let addr = leaked_lock_addr();
        wedge_with_dead_owner(addr, dead_pid());

        // Before the fix read_begin spins forever on the odd seq; recovery unwedges it.
        run_or_fail("read_begin", Duration::from_secs(5), move || {
            let _ = lock_at(addr).read_begin();
        });
    }

    #[test]
    fn recovers_when_dead_owner_left_seq_even() {
        // A dead owner can leave the lock held with seq EITHER parity: odd if it
        // died mid data-write, even if it died inside write_unlock after seq→even
        // but before clearing the owner — and identically, this is the state a
        // *recoverer* leaves if it itself dies after stamping its (now-dead) PID.
        // Because the owner is a real, probeable PID (never a terminal sentinel),
        // recovery must work regardless of seq parity. The odd case is covered
        // above; this pins the even case so it can never wedge (#38).
        let addr = leaked_lock_addr();
        let dead = dead_pid();
        let l = lock_at(addr);
        unsafe {
            (*l.seq_ptr).store(2, Ordering::Release); // even: no writer mid-data
            (*l.write_lock_ptr).store(dead, Ordering::Release); // but owner never cleared
        }
        run_or_fail("write_lock (seq even)", Duration::from_secs(5), move || {
            let lock = lock_at(addr);
            let _g = lock.write_lock();
        });
        let l = lock_at(addr);
        assert_eq!(
            unsafe { &*l.write_lock_ptr }.load(Ordering::Relaxed),
            0,
            "lock not released after recovering an even-seq dead owner"
        );
        assert_eq!(
            unsafe { &*l.seq_ptr }.load(Ordering::Relaxed) & 1,
            0,
            "seq left odd after recovery"
        );
    }

    #[test]
    fn live_owner_is_never_stolen() {
        // owner_is_dead() is the single gate that keeps recovery from stealing a
        // lock held by a running process: write_lock()/read_begin() only call
        // recover_from_dead() when this returns true. It must report a live process
        // (this one) as alive, and must never probe a value that maps to a
        // non-positive `pid_t` — `kill(0, ..)` / `kill(-1, ..)` would target whole
        // process groups / every signalable process.
        assert!(!owner_is_dead(current_pid()), "live process reported dead");
        assert!(!owner_is_dead(0), "free slot treated as a dead owner");
        // u32::MAX as i32 == -1 (kill(-1) = every process); i32::MAX+1 as i32 < 0.
        assert!(
            !owner_is_dead(u32::MAX),
            "value mapping to kill(-1) was probed"
        );
        assert!(
            !owner_is_dead(i32::MAX as u32 + 1),
            "value mapping to a negative pid was probed"
        );
    }

    #[test]
    fn panic_in_critical_section_releases_lock() {
        let addr = leaked_lock_addr();
        let r = std::panic::catch_unwind(move || {
            let lock = lock_at(addr);
            let _g = lock.write_lock();
            panic!("boom in critical section");
        });
        assert!(r.is_err(), "panic should propagate");

        let l = lock_at(addr);
        assert_eq!(
            unsafe { &*l.write_lock_ptr }.load(Ordering::Relaxed),
            0,
            "lock leaked after a panic in the critical section"
        );
        assert_eq!(
            unsafe { &*l.seq_ptr }.load(Ordering::Relaxed) & 1,
            0,
            "seq left odd after a panic — parity not restored"
        );
    }
}
