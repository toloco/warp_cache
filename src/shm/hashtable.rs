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
