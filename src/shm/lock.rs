/// Cross-process read-write lock using POSIX pthread_rwlock with
/// PTHREAD_PROCESS_SHARED attribute.
///
/// The lock lives in shared memory (mmap) so it's accessible from
/// multiple processes. On macOS, pthread_rwlock PROCESS_SHARED is
/// supported since macOS 10.4.
use std::io;

/// Size reserved for the lock in the mmap region.
/// pthread_rwlock_t is 56 bytes on x86_64 Linux, 200 bytes on macOS arm64.
/// We over-allocate to be safe.
pub const LOCK_SIZE: usize = 256;

/// A handle to a cross-process rwlock stored in shared memory.
pub struct ShmRwLock {
    /// Raw pointer to the pthread_rwlock_t in the mmap region.
    lock_ptr: *mut libc::pthread_rwlock_t,
}

unsafe impl Send for ShmRwLock {}
unsafe impl Sync for ShmRwLock {}

impl ShmRwLock {
    /// Initialize a new rwlock at the given memory location.
    ///
    /// # Safety
    /// `ptr` must point to at least `size_of::<pthread_rwlock_t>()` bytes
    /// of shared memory that is zeroed or uninitialized.
    pub unsafe fn init(ptr: *mut u8) -> io::Result<Self> {
        let lock_ptr = ptr as *mut libc::pthread_rwlock_t;

        let mut attr: libc::pthread_rwlockattr_t = std::mem::zeroed();
        let ret = libc::pthread_rwlockattr_init(&mut attr);
        if ret != 0 {
            return Err(io::Error::from_raw_os_error(ret));
        }

        let ret = libc::pthread_rwlockattr_setpshared(&mut attr, libc::PTHREAD_PROCESS_SHARED);
        if ret != 0 {
            libc::pthread_rwlockattr_destroy(&mut attr);
            return Err(io::Error::from_raw_os_error(ret));
        }

        let ret = libc::pthread_rwlock_init(lock_ptr, &attr);
        libc::pthread_rwlockattr_destroy(&mut attr);
        if ret != 0 {
            return Err(io::Error::from_raw_os_error(ret));
        }

        Ok(ShmRwLock { lock_ptr })
    }

    /// Attach to an already-initialized rwlock at the given memory location.
    ///
    /// # Safety
    /// `ptr` must point to a previously initialized `pthread_rwlock_t`
    /// in shared memory.
    pub unsafe fn from_existing(ptr: *mut u8) -> Self {
        ShmRwLock {
            lock_ptr: ptr as *mut libc::pthread_rwlock_t,
        }
    }

    /// Acquire a read lock. Blocks until available.
    pub fn read_lock(&self) {
        unsafe {
            let ret = libc::pthread_rwlock_rdlock(self.lock_ptr);
            debug_assert_eq!(ret, 0, "pthread_rwlock_rdlock failed: {ret}");
        }
    }

    /// Release a read lock.
    pub fn read_unlock(&self) {
        unsafe {
            let ret = libc::pthread_rwlock_unlock(self.lock_ptr);
            debug_assert_eq!(ret, 0, "pthread_rwlock_unlock failed: {ret}");
        }
    }

    /// Acquire a write lock. Blocks until available.
    pub fn write_lock(&self) {
        unsafe {
            let ret = libc::pthread_rwlock_wrlock(self.lock_ptr);
            debug_assert_eq!(ret, 0, "pthread_rwlock_wrlock failed: {ret}");
        }
    }

    /// Release a write lock.
    pub fn write_unlock(&self) {
        unsafe {
            let ret = libc::pthread_rwlock_unlock(self.lock_ptr);
            debug_assert_eq!(ret, 0, "pthread_rwlock_unlock failed: {ret}");
        }
    }

    /// Destroy the rwlock. Only call when no other process is using it.
    #[allow(dead_code)]
    pub unsafe fn destroy(&self) {
        libc::pthread_rwlock_destroy(self.lock_ptr);
    }
}
