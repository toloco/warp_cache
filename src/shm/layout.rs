/// `#[repr(C)]` structures that live in shared memory (mmap).
///
/// All structs use fixed-size fields and explicit padding so the
/// layout is identical across compilations and processes.

/// Magic bytes at the start of the header to validate the mapping.
pub const MAGIC: [u8; 8] = *b"FCACHE01";

/// Size of the fixed header at the start of the region.
pub const HEADER_SIZE: usize = 256;

/// Sentinel value meaning "no slot" in prev/next linked-list pointers.
pub const SLOT_NONE: i32 = -1;

/// Sentinel value meaning "empty bucket" in the hash table.
pub const BUCKET_EMPTY: i32 = -1;

/// Header lives at offset 0 of the mmap region.
///
/// Fields are ordered u64-first to avoid implicit alignment padding
/// in `#[repr(C)]`.
#[repr(C)]
#[derive(Debug)]
pub struct Header {
    // 8-byte aligned group
    pub magic: [u8; 8],      // 0..8
    pub ttl_nanos: u64,      // 8..16   (0 = no TTL)
    pub hits: u64,           // 16..24
    pub misses: u64,         // 24..32
    pub oversize_skips: u64, // 32..40

    // 4-byte aligned group
    pub version: u32,        // 40..44
    pub strategy: u32,       // 44..48  (0=LRU, 1=MRU, 2=FIFO, 3=LFU)
    pub capacity: u32,       // 48..52  (max_size)
    pub ht_capacity: u32,    // 52..56  (hash-table bucket count)
    pub slot_size: u32,      // 56..60
    pub max_key_size: u32,   // 60..64
    pub max_value_size: u32, // 64..68
    pub current_size: u32,   // 68..72
    pub list_head: i32,      // 72..76  (eviction list, SLOT_NONE = empty)
    pub list_tail: i32,      // 76..80
    pub free_head: i32,      // 80..84
    pub _reserved: i32,      // 84..88  (alignment padding)

    // Explicit padding to 256 bytes: 256 - 88 = 168
    pub _pad: [u8; 168],
}

// Compile-time assertion that Header is exactly HEADER_SIZE bytes.
const _: () = assert!(std::mem::size_of::<Header>() == HEADER_SIZE);

/// One bucket in the open-addressing hash table.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Bucket {
    pub hash: u64,
    pub slot_index: i32,
    pub _pad: u32,
}

impl Bucket {
    pub const SIZE: usize = std::mem::size_of::<Self>();
}

const _: () = assert!(std::mem::size_of::<Bucket>() == 16);

/// Per-slot header inside the slab arena. Followed by key_bytes then value_bytes.
pub const SLOT_HEADER_SIZE: usize = 64;

/// Fields ordered u64-first to avoid implicit alignment padding.
#[repr(C)]
#[derive(Debug)]
pub struct SlotHeader {
    // 8-byte aligned group
    pub key_hash: u64,         // 0..8
    pub created_at_nanos: u64, // 8..16  (monotonic nanos)
    pub frequency: u64,        // 16..24
    pub unique_id: u64,        // 24..32 (monotonic ID for LFU)

    // 4-byte aligned group
    pub occupied: u32,  // 32..36 (1 = occupied, 0 = free)
    pub key_len: u32,   // 36..40
    pub value_len: u32, // 40..44
    pub prev: i32,      // 44..48 (eviction list previous)
    pub next: i32,      // 48..52 (eviction list next)

    // Explicit padding to 64 bytes: 64 - 52 = 12
    pub _pad: [u8; 12],
}

const _: () = assert!(std::mem::size_of::<SlotHeader>() == SLOT_HEADER_SIZE);

/// Compute the total size of the mmap region.
pub fn region_size(capacity: u32, ht_capacity: u32, slot_size: u32) -> usize {
    HEADER_SIZE + (ht_capacity as usize * Bucket::SIZE) + (capacity as usize * slot_size as usize)
}

/// Offset of the hash-table array from the start of the region.
pub fn ht_offset() -> usize {
    HEADER_SIZE
}

/// Offset of the slab arena from the start of the region.
pub fn slab_offset(ht_capacity: u32) -> usize {
    HEADER_SIZE + (ht_capacity as usize * Bucket::SIZE)
}
