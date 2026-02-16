/// Open-addressing hash table operating on raw shared memory bytes.
///
/// Uses linear probing. The table is sized at 2× capacity to keep
/// load factor under 50%.
use super::layout::{Bucket, BUCKET_EMPTY};

/// Look up a key hash in the hash table, returning the slot index if found.
///
/// Compares the stored serialized key bytes against `key_bytes` via memcmp
/// to confirm the match (hashes can collide).
///
/// # Safety
/// `ht_base` must point to a valid hash table region of `ht_capacity` buckets.
/// `slab_base` must point to a valid slab arena.
/// `slot_size` must be the correct slot size.
pub unsafe fn ht_lookup(
    ht_base: *const u8,
    ht_capacity: u32,
    slab_base: *const u8,
    slot_size: u32,
    key_hash: u64,
    key_bytes: &[u8],
) -> Option<i32> {
    let mask = ht_capacity.wrapping_sub(1);
    let mut idx = (key_hash as u32) & mask;

    for _ in 0..ht_capacity {
        let bucket = &*(ht_base.add(idx as usize * Bucket::SIZE) as *const Bucket);

        if bucket.slot_index == BUCKET_EMPTY {
            return None; // empty bucket → key not present
        }

        if bucket.hash == key_hash {
            // Check actual key bytes
            let slot_ptr = slab_base.add(bucket.slot_index as usize * slot_size as usize);
            let slot_header = &*(slot_ptr as *const super::layout::SlotHeader);

            if slot_header.occupied != 0 && slot_header.key_len == key_bytes.len() as u32 {
                let stored_key = std::slice::from_raw_parts(
                    slot_ptr.add(super::layout::SLOT_HEADER_SIZE),
                    slot_header.key_len as usize,
                );
                if stored_key == key_bytes {
                    return Some(bucket.slot_index);
                }
            }
        }

        idx = (idx + 1) & mask;
    }

    None // table full (shouldn't happen with 50% load)
}

/// Insert a mapping from `key_hash` → `slot_index` into the hash table.
///
/// # Safety
/// Same requirements as `ht_lookup`.
pub unsafe fn ht_insert(ht_base: *mut u8, ht_capacity: u32, key_hash: u64, slot_index: i32) {
    let mask = ht_capacity.wrapping_sub(1);
    let mut idx = (key_hash as u32) & mask;

    for _ in 0..ht_capacity {
        let bucket = &mut *(ht_base.add(idx as usize * Bucket::SIZE) as *mut Bucket);

        if bucket.slot_index == BUCKET_EMPTY {
            bucket.hash = key_hash;
            bucket.slot_index = slot_index;
            return;
        }

        idx = (idx + 1) & mask;
    }

    // Table full — should never happen because we size at 2× capacity
    debug_assert!(false, "hash table is full");
}

/// Remove the entry matching `key_hash` + `key_bytes` from the hash table.
///
/// Uses backward-shift deletion to maintain linear-probing invariant.
///
/// # Safety
/// Same requirements as `ht_lookup`.
pub unsafe fn ht_remove(
    ht_base: *mut u8,
    ht_capacity: u32,
    slab_base: *const u8,
    slot_size: u32,
    key_hash: u64,
    key_bytes: &[u8],
) -> bool {
    let mask = ht_capacity.wrapping_sub(1);
    let mut idx = (key_hash as u32) & mask;

    // Find the bucket to remove
    let mut found_idx = None;
    for _ in 0..ht_capacity {
        let bucket = &*(ht_base.add(idx as usize * Bucket::SIZE) as *const Bucket);

        if bucket.slot_index == BUCKET_EMPTY {
            return false;
        }

        if bucket.hash == key_hash {
            let slot_ptr = slab_base.add(bucket.slot_index as usize * slot_size as usize);
            let slot_header = &*(slot_ptr as *const super::layout::SlotHeader);

            if slot_header.key_len == key_bytes.len() as u32 {
                let stored_key = std::slice::from_raw_parts(
                    slot_ptr.add(super::layout::SLOT_HEADER_SIZE),
                    slot_header.key_len as usize,
                );
                if stored_key == key_bytes {
                    found_idx = Some(idx);
                    break;
                }
            }
        }

        idx = (idx + 1) & mask;
    }

    let remove_idx = match found_idx {
        Some(i) => i,
        None => return false,
    };

    // Backward-shift deletion
    let mut empty = remove_idx;
    let mut j = (empty + 1) & mask;

    loop {
        let bucket_j = &*(ht_base.add(j as usize * Bucket::SIZE) as *const Bucket);

        if bucket_j.slot_index == BUCKET_EMPTY {
            break;
        }

        // Check if bucket_j's ideal position is at or before `empty`
        let ideal = (bucket_j.hash as u32) & mask;
        let should_move = if empty <= j {
            ideal <= empty || ideal > j
        } else {
            ideal <= empty && ideal > j
        };

        if should_move {
            // Copy bucket_j to empty
            let src = &*(ht_base.add(j as usize * Bucket::SIZE) as *const Bucket);
            let dst = &mut *(ht_base.add(empty as usize * Bucket::SIZE) as *mut Bucket);
            dst.hash = src.hash;
            dst.slot_index = src.slot_index;
            empty = j;
        }

        j = (j + 1) & mask;
    }

    // Clear the final empty slot
    let bucket = &mut *(ht_base.add(empty as usize * Bucket::SIZE) as *mut Bucket);
    bucket.hash = 0;
    bucket.slot_index = BUCKET_EMPTY;

    true
}

/// Clear all buckets in the hash table.
///
/// # Safety
/// `ht_base` must point to a valid hash table region.
pub unsafe fn ht_clear(ht_base: *mut u8, ht_capacity: u32) {
    for i in 0..ht_capacity as usize {
        let bucket = &mut *(ht_base.add(i * Bucket::SIZE) as *mut Bucket);
        bucket.hash = 0;
        bucket.slot_index = BUCKET_EMPTY;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shm::layout::{Bucket, SLOT_HEADER_SIZE};

    const TEST_SLOT_SIZE: u32 = 128;

    /// Create a hash table buffer with all buckets initialised to BUCKET_EMPTY.
    fn make_ht(capacity: u32) -> Vec<u8> {
        let size = capacity as usize * Bucket::SIZE;
        let mut buf = vec![0u8; size];
        unsafe { ht_clear(buf.as_mut_ptr(), capacity) };
        buf
    }

    /// Create a zeroed slab buffer for `num_slots` slots.
    fn make_slab(num_slots: u32) -> Vec<u8> {
        vec![0u8; num_slots as usize * TEST_SLOT_SIZE as usize]
    }

    /// Write a SlotHeader + key bytes into the slab at the given slot index.
    ///
    /// Uses byte-level writes matching the `#[repr(C)]` SlotHeader layout:
    ///   0..8   key_hash (u64)
    ///   32..36 occupied (u32)
    ///   36..40 key_len  (u32)
    ///   64..   key bytes
    fn write_slot(slab: &mut [u8], slot_index: u32, key_hash: u64, key_bytes: &[u8]) {
        let off = slot_index as usize * TEST_SLOT_SIZE as usize;
        slab[off..off + 8].copy_from_slice(&key_hash.to_ne_bytes());
        slab[off + 32..off + 36].copy_from_slice(&1u32.to_ne_bytes()); // occupied = 1
        slab[off + 36..off + 40].copy_from_slice(&(key_bytes.len() as u32).to_ne_bytes());
        slab[off + SLOT_HEADER_SIZE..off + SLOT_HEADER_SIZE + key_bytes.len()]
            .copy_from_slice(key_bytes);
    }

    #[test]
    fn insert_and_lookup() {
        let cap: u32 = 8;
        let mut ht = make_ht(cap);
        let mut slab = make_slab(cap);

        write_slot(&mut slab, 0, 42, b"hello");

        unsafe {
            ht_insert(ht.as_mut_ptr(), cap, 42, 0);
            let result = ht_lookup(
                ht.as_ptr(),
                cap,
                slab.as_ptr(),
                TEST_SLOT_SIZE,
                42,
                b"hello",
            );
            assert_eq!(result, Some(0));
        }
    }

    #[test]
    fn lookup_missing() {
        let cap: u32 = 8;
        let mut ht = make_ht(cap);
        let mut slab = make_slab(cap);

        // Empty table
        unsafe {
            assert_eq!(
                ht_lookup(ht.as_ptr(), cap, slab.as_ptr(), TEST_SLOT_SIZE, 99, b"nope"),
                None
            );
        }

        // Insert one key, look up a different one
        write_slot(&mut slab, 0, 42, b"hello");
        unsafe {
            ht_insert(ht.as_mut_ptr(), cap, 42, 0);
            assert_eq!(
                ht_lookup(
                    ht.as_ptr(),
                    cap,
                    slab.as_ptr(),
                    TEST_SLOT_SIZE,
                    99,
                    b"world"
                ),
                None
            );
        }
    }

    #[test]
    fn collision_probing() {
        let cap: u32 = 8; // mask = 7
        let mut ht = make_ht(cap);
        let mut slab = make_slab(cap);

        // Both hashes map to bucket 0: 0x10 & 7 = 0, 0x08 & 7 = 0
        let hash_a: u64 = 0x10;
        let hash_b: u64 = 0x08;

        write_slot(&mut slab, 0, hash_a, b"aaa");
        write_slot(&mut slab, 1, hash_b, b"bbb");

        unsafe {
            ht_insert(ht.as_mut_ptr(), cap, hash_a, 0);
            ht_insert(ht.as_mut_ptr(), cap, hash_b, 1);

            assert_eq!(
                ht_lookup(
                    ht.as_ptr(),
                    cap,
                    slab.as_ptr(),
                    TEST_SLOT_SIZE,
                    hash_a,
                    b"aaa"
                ),
                Some(0)
            );
            assert_eq!(
                ht_lookup(
                    ht.as_ptr(),
                    cap,
                    slab.as_ptr(),
                    TEST_SLOT_SIZE,
                    hash_b,
                    b"bbb"
                ),
                Some(1)
            );
        }
    }

    #[test]
    fn remove_simple() {
        let cap: u32 = 8;
        let mut ht = make_ht(cap);
        let mut slab = make_slab(cap);

        write_slot(&mut slab, 0, 42, b"hello");

        unsafe {
            ht_insert(ht.as_mut_ptr(), cap, 42, 0);
            assert!(ht_remove(
                ht.as_mut_ptr(),
                cap,
                slab.as_ptr(),
                TEST_SLOT_SIZE,
                42,
                b"hello"
            ));
            assert_eq!(
                ht_lookup(
                    ht.as_ptr(),
                    cap,
                    slab.as_ptr(),
                    TEST_SLOT_SIZE,
                    42,
                    b"hello"
                ),
                None
            );
        }
    }

    #[test]
    fn remove_missing() {
        let cap: u32 = 8;
        let mut ht = make_ht(cap);
        let slab = make_slab(cap);

        unsafe {
            assert!(!ht_remove(
                ht.as_mut_ptr(),
                cap,
                slab.as_ptr(),
                TEST_SLOT_SIZE,
                99,
                b"nope"
            ));
        }
    }

    #[test]
    fn remove_backward_shift() {
        let cap: u32 = 8; // mask = 7
        let mut ht = make_ht(cap);
        let mut slab = make_slab(cap);

        // Both map to bucket 0
        let hash_a: u64 = 0x10; // 0x10 & 7 = 0
        let hash_b: u64 = 0x08; // 0x08 & 7 = 0

        write_slot(&mut slab, 0, hash_a, b"aaa");
        write_slot(&mut slab, 1, hash_b, b"bbb");

        unsafe {
            ht_insert(ht.as_mut_ptr(), cap, hash_a, 0); // → bucket 0
            ht_insert(ht.as_mut_ptr(), cap, hash_b, 1); // → bucket 1 (probed)

            // Remove A — backward shift should move B back to bucket 0
            assert!(ht_remove(
                ht.as_mut_ptr(),
                cap,
                slab.as_ptr(),
                TEST_SLOT_SIZE,
                hash_a,
                b"aaa"
            ));

            // B must still be findable
            assert_eq!(
                ht_lookup(
                    ht.as_ptr(),
                    cap,
                    slab.as_ptr(),
                    TEST_SLOT_SIZE,
                    hash_b,
                    b"bbb"
                ),
                Some(1)
            );
        }
    }

    #[test]
    fn clear() {
        let cap: u32 = 8;
        let mut ht = make_ht(cap);
        let mut slab = make_slab(cap);

        write_slot(&mut slab, 0, 10, b"aaa");
        write_slot(&mut slab, 1, 20, b"bbb");
        write_slot(&mut slab, 2, 30, b"ccc");

        unsafe {
            ht_insert(ht.as_mut_ptr(), cap, 10, 0);
            ht_insert(ht.as_mut_ptr(), cap, 20, 1);
            ht_insert(ht.as_mut_ptr(), cap, 30, 2);

            ht_clear(ht.as_mut_ptr(), cap);

            assert_eq!(
                ht_lookup(ht.as_ptr(), cap, slab.as_ptr(), TEST_SLOT_SIZE, 10, b"aaa"),
                None
            );
            assert_eq!(
                ht_lookup(ht.as_ptr(), cap, slab.as_ptr(), TEST_SLOT_SIZE, 20, b"bbb"),
                None
            );
            assert_eq!(
                ht_lookup(ht.as_ptr(), cap, slab.as_ptr(), TEST_SLOT_SIZE, 30, b"ccc"),
                None
            );
        }
    }

    #[test]
    fn near_capacity_stress() {
        let cap: u32 = 16; // mask = 15
        let mut ht = make_ht(cap);
        let mut slab = make_slab(cap);

        // 7 entries including deliberate collisions (hashes sharing lower bits)
        let entries: &[(u64, &[u8])] = &[
            (1, b"k1"),
            (17, b"k2"), // 17 & 15 = 1, collides with k1
            (33, b"k3"), // 33 & 15 = 1, collides again
            (2, b"k4"),
            (18, b"k5"), // 18 & 15 = 2, collides with k4
            (5, b"k6"),
            (100, b"k7"), // 100 & 15 = 4
        ];

        for (i, &(hash, key)) in entries.iter().enumerate() {
            write_slot(&mut slab, i as u32, hash, key);
        }

        unsafe {
            for (i, &(hash, _)) in entries.iter().enumerate() {
                ht_insert(ht.as_mut_ptr(), cap, hash, i as i32);
            }

            // All entries must be findable
            for (i, &(hash, key)) in entries.iter().enumerate() {
                assert_eq!(
                    ht_lookup(ht.as_ptr(), cap, slab.as_ptr(), TEST_SLOT_SIZE, hash, key),
                    Some(i as i32),
                    "entry {i} not found"
                );
            }
        }
    }
}
