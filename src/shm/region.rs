/// Shared memory region management using mmap.
///
/// Creates or opens a named memory-mapped file that holds the entire
/// cache: header + lock + hash table + slab arena.
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use memmap2::MmapMut;

use super::layout::{self, Bucket, Header, SlotHeader, BUCKET_EMPTY, MAGIC, SLOT_NONE};
use super::lock::{ShmRwLock, LOCK_SIZE};

/// Where to store the mmap files.
fn shm_dir() -> PathBuf {
    if cfg!(target_os = "linux") {
        PathBuf::from("/dev/shm")
    } else {
        // macOS and other Unix: use TMPDIR
        std::env::temp_dir().join("fast_cache")
    }
}

/// The full shared-memory region, owning the mmap handle and providing
/// raw accessors to the structures within.
#[allow(dead_code)]
pub struct ShmRegion {
    pub mmap: MmapMut,
    pub path: PathBuf,
    pub lock_mmap: MmapMut,
    pub lock_path: PathBuf,
}

impl ShmRegion {
    /// Create a new shared memory region, initializing all structures.
    pub fn create(
        name: &str,
        strategy: u32,
        capacity: u32,
        slot_size: u32,
        max_key_size: u32,
        max_value_size: u32,
        ttl_nanos: u64,
    ) -> io::Result<Self> {
        let dir = shm_dir();
        if !dir.exists() {
            fs::create_dir_all(&dir)?;
        }

        // Hash table must be power-of-2 for bitmask probing
        let ht_capacity = (capacity * 2).next_power_of_two();
        let total_size = layout::region_size(capacity, ht_capacity, slot_size);

        let data_path = dir.join(format!("{name}.data"));
        let lock_path = dir.join(format!("{name}.lock"));

        // Create or truncate the data file
        let data_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&data_path)?;
        data_file.set_len(total_size as u64)?;

        // Create or truncate the lock file
        let lock_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&lock_path)?;
        lock_file.set_len(LOCK_SIZE as u64)?;

        // Safety: we just created these files and own them exclusively at this point.
        let mut mmap = unsafe { MmapMut::map_mut(&data_file)? };
        let mut lock_mmap = unsafe { MmapMut::map_mut(&lock_file)? };

        // Zero the entire region
        mmap.fill(0);
        lock_mmap.fill(0);

        // Initialize header
        let header = unsafe { &mut *(mmap.as_mut_ptr() as *mut Header) };
        header.magic = MAGIC;
        header.version = 1;
        header.strategy = strategy;
        header.capacity = capacity;
        header.ht_capacity = ht_capacity;
        header.slot_size = slot_size;
        header.max_key_size = max_key_size;
        header.max_value_size = max_value_size;
        header.ttl_nanos = ttl_nanos;
        header.hits = 0;
        header.misses = 0;
        header.oversize_skips = 0;
        header.current_size = 0;
        header.list_head = SLOT_NONE;
        header.list_tail = SLOT_NONE;
        header.free_head = 0; // first slot is start of free list

        // Initialize hash table buckets to empty
        let ht_base = layout::ht_offset();
        for i in 0..ht_capacity as usize {
            let offset = ht_base + i * Bucket::SIZE;
            let bucket = unsafe { &mut *(mmap.as_mut_ptr().add(offset) as *mut Bucket) };
            bucket.hash = 0;
            bucket.slot_index = BUCKET_EMPTY;
        }

        // Initialize slab free list: each slot's next points to the next slot
        let slab_base = layout::slab_offset(ht_capacity);
        for i in 0..capacity as usize {
            let offset = slab_base + i * slot_size as usize;
            let slot = unsafe { &mut *(mmap.as_mut_ptr().add(offset) as *mut SlotHeader) };
            slot.occupied = 0;
            slot.prev = SLOT_NONE;
            slot.next = if i + 1 < capacity as usize {
                (i + 1) as i32
            } else {
                SLOT_NONE
            };
        }

        // Initialize the cross-process rwlock in the lock region
        unsafe {
            ShmRwLock::init(lock_mmap.as_mut_ptr())?;
        }

        mmap.flush()?;
        lock_mmap.flush()?;

        Ok(ShmRegion {
            mmap,
            path: data_path,
            lock_mmap,
            lock_path,
        })
    }

    /// Open an existing shared memory region.
    #[allow(dead_code)]
    pub fn open(name: &str) -> io::Result<Self> {
        let dir = shm_dir();
        let data_path = dir.join(format!("{name}.data"));
        let lock_path = dir.join(format!("{name}.lock"));

        Self::open_paths(&data_path, &lock_path)
    }

    fn open_paths(data_path: &Path, lock_path: &Path) -> io::Result<ShmRegion> {
        let data_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(data_path)?;

        let lock_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock_path)?;

        let mmap = unsafe { MmapMut::map_mut(&data_file)? };
        let lock_mmap = unsafe { MmapMut::map_mut(&lock_file)? };

        // Validate magic
        let header = unsafe { &*(mmap.as_ptr() as *const Header) };
        if header.magic != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid shared cache file: bad magic",
            ));
        }

        Ok(ShmRegion {
            mmap,
            path: data_path.to_path_buf(),
            lock_mmap,
            lock_path: lock_path.to_path_buf(),
        })
    }

    /// Create if doesn't exist, otherwise open.
    pub fn create_or_open(
        name: &str,
        strategy: u32,
        capacity: u32,
        slot_size: u32,
        max_key_size: u32,
        max_value_size: u32,
        ttl_nanos: u64,
    ) -> io::Result<Self> {
        let dir = shm_dir();
        let data_path = dir.join(format!("{name}.data"));
        let lock_path = dir.join(format!("{name}.lock"));

        if data_path.exists() && lock_path.exists() {
            match Self::open_paths(&data_path, &lock_path) {
                Ok(region) => {
                    // Validate parameters match
                    let header = region.header();
                    if header.capacity == capacity
                        && header.strategy == strategy
                        && header.max_key_size == max_key_size
                        && header.max_value_size == max_value_size
                    {
                        return Ok(region);
                    }
                    // Parameters don't match — recreate
                    drop(region);
                }
                Err(_) => {
                    // Stale or corrupted file — recreate
                }
            }
        }

        Self::create(
            name,
            strategy,
            capacity,
            slot_size,
            max_key_size,
            max_value_size,
            ttl_nanos,
        )
    }

    pub fn header(&self) -> &Header {
        unsafe { &*(self.mmap.as_ptr() as *const Header) }
    }

    #[allow(dead_code)]
    pub fn header_mut(&mut self) -> &mut Header {
        unsafe { &mut *(self.mmap.as_mut_ptr() as *mut Header) }
    }

    pub fn lock(&self) -> ShmRwLock {
        unsafe { ShmRwLock::from_existing(self.lock_mmap.as_ptr() as *mut u8) }
    }

    pub fn base_ptr(&self) -> *const u8 {
        self.mmap.as_ptr()
    }

    #[allow(dead_code)]
    pub fn base_mut_ptr(&mut self) -> *mut u8 {
        self.mmap.as_mut_ptr()
    }

    /// Remove the backing files.
    #[allow(dead_code)]
    pub fn unlink(&self) -> io::Result<()> {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(&self.lock_path);
        Ok(())
    }
}
