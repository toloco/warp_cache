/// Intrusive doubly-linked list for eviction ordering.
///
/// Uses prev/next indices stored in each slot header.
/// Supports LRU, MRU, FIFO, and LFU eviction strategies.
use super::layout::{Header, SlotHeader, SLOT_NONE};

/// Get a reference to a slot header.
///
/// # Safety
/// `slab_base` must be a valid slab arena pointer, `index` must be in range.
unsafe fn slot(slab_base: *const u8, slot_size: u32, index: i32) -> &'static SlotHeader {
    &*(slab_base.add(index as usize * slot_size as usize) as *const SlotHeader)
}

/// Get a mutable reference to a slot header.
unsafe fn slot_mut(slab_base: *mut u8, slot_size: u32, index: i32) -> &'static mut SlotHeader {
    &mut *(slab_base.add(index as usize * slot_size as usize) as *mut SlotHeader)
}

/// Remove a slot from the eviction linked list.
///
/// # Safety
/// Caller must hold write lock. `slab_base` and `header` must be valid.
pub unsafe fn list_remove(header: &mut Header, slab_base: *mut u8, slot_size: u32, index: i32) {
    let s = slot(slab_base, slot_size, index);
    let prev = s.prev;
    let next = s.next;

    if prev != SLOT_NONE {
        slot_mut(slab_base, slot_size, prev).next = next;
    } else {
        header.list_head = next;
    }

    if next != SLOT_NONE {
        slot_mut(slab_base, slot_size, next).prev = prev;
    } else {
        header.list_tail = prev;
    }

    let s = slot_mut(slab_base, slot_size, index);
    s.prev = SLOT_NONE;
    s.next = SLOT_NONE;
}

/// Push a slot to the tail of the eviction list (most recently used position).
///
/// # Safety
/// Caller must hold write lock.
pub unsafe fn list_push_tail(header: &mut Header, slab_base: *mut u8, slot_size: u32, index: i32) {
    let s = slot_mut(slab_base, slot_size, index);
    s.prev = header.list_tail;
    s.next = SLOT_NONE;

    if header.list_tail != SLOT_NONE {
        slot_mut(slab_base, slot_size, header.list_tail).next = index;
    } else {
        header.list_head = index;
    }

    header.list_tail = index;
}

/// Move a slot to the tail of the list (touch for LRU/MRU).
///
/// # Safety
/// Caller must hold write lock.
pub unsafe fn list_move_to_tail(
    header: &mut Header,
    slab_base: *mut u8,
    slot_size: u32,
    index: i32,
) {
    list_remove(header, slab_base, slot_size, index);
    list_push_tail(header, slab_base, slot_size, index);
}

/// For LFU: insert a slot in sorted position by (frequency ASC, unique_id ASC).
///
/// Scans from the tail (highest frequency) toward head.
///
/// # Safety
/// Caller must hold write lock.
pub unsafe fn list_insert_lfu(header: &mut Header, slab_base: *mut u8, slot_size: u32, index: i32) {
    let new_slot = slot(slab_base, slot_size, index);
    let new_freq = new_slot.frequency;
    let new_uid = new_slot.unique_id;

    // Find insertion point: scan from tail backward
    let mut cursor = header.list_tail;
    while cursor != SLOT_NONE {
        let cs = slot(slab_base, slot_size, cursor);
        // Insert after cursor if cursor's freq < new_freq,
        // or (same freq and cursor's uid < new_uid)
        if cs.frequency < new_freq || (cs.frequency == new_freq && cs.unique_id <= new_uid) {
            // Insert after cursor
            let s = slot_mut(slab_base, slot_size, index);
            s.prev = cursor;
            s.next = slot(slab_base, slot_size, cursor).next;

            if s.next != SLOT_NONE {
                slot_mut(slab_base, slot_size, s.next).prev = index;
            } else {
                header.list_tail = index;
            }

            slot_mut(slab_base, slot_size, cursor).next = index;
            return;
        }
        cursor = cs.prev;
    }

    // Insert at head
    let s = slot_mut(slab_base, slot_size, index);
    s.prev = SLOT_NONE;
    s.next = header.list_head;

    if header.list_head != SLOT_NONE {
        slot_mut(slab_base, slot_size, header.list_head).prev = index;
    } else {
        header.list_tail = index;
    }

    header.list_head = index;
}

/// Pick the slot to evict based on the strategy.
///
/// Returns the slot index to evict, or SLOT_NONE if the list is empty.
///
/// - LRU (0): evict head (least recently used)
/// - MRU (1): evict tail (most recently used)
/// - FIFO (2): evict head (oldest insertion)
/// - LFU (3): evict head (lowest frequency — list is sorted)
pub fn evict_candidate(header: &Header, strategy: u32) -> i32 {
    match strategy {
        0 | 2 | 3 => header.list_head, // LRU, FIFO, LFU: evict from head
        1 => header.list_tail,         // MRU: evict from tail
        _ => header.list_head,
    }
}

/// Called on cache hit to update ordering.
///
/// - LRU: move to tail
/// - MRU: move to tail
/// - FIFO: no-op (insertion order preserved)
/// - LFU: increment frequency, reposition in sorted list
///
/// # Safety
/// Caller must hold write lock.
pub unsafe fn on_access(
    header: &mut Header,
    slab_base: *mut u8,
    slot_size: u32,
    index: i32,
    strategy: u32,
) {
    match strategy {
        0 | 1 => {
            // LRU/MRU: move to tail
            list_move_to_tail(header, slab_base, slot_size, index);
        }
        2 => {
            // FIFO: no reordering on access
        }
        3 => {
            // LFU: increment frequency and reposition
            let s = slot_mut(slab_base, slot_size, index);
            s.frequency += 1;
            list_remove(header, slab_base, slot_size, index);
            list_insert_lfu(header, slab_base, slot_size, index);
        }
        _ => {}
    }
}

/// Called on insert to add the new slot to the eviction list.
///
/// # Safety
/// Caller must hold write lock.
pub unsafe fn on_insert(
    header: &mut Header,
    slab_base: *mut u8,
    slot_size: u32,
    index: i32,
    strategy: u32,
) {
    match strategy {
        0..=2 => {
            // LRU/MRU/FIFO: append to tail
            list_push_tail(header, slab_base, slot_size, index);
        }
        3 => {
            // LFU: insert in sorted position (frequency = 0 → near head)
            list_insert_lfu(header, slab_base, slot_size, index);
        }
        _ => {
            list_push_tail(header, slab_base, slot_size, index);
        }
    }
}
