/// Intrusive doubly-linked list and SIEVE eviction for shared memory.
///
/// Uses prev/next indices stored in each slot header.
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

/// Push a slot to the tail of the eviction list.
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

/// SIEVE eviction: find a victim slot to evict.
///
/// Scans from `header.sieve_hand` (or `list_head` if SLOT_NONE).
/// - If `visited == 1`: clear to 0, advance hand.
/// - If `visited == 0`: set hand to next, return this index as victim.
///
/// Returns the slot index to evict, or `SLOT_NONE` if the list is empty.
///
/// # Safety
/// Caller must hold write lock.
pub unsafe fn sieve_evict(header: &mut Header, slab_base: *mut u8, slot_size: u32) -> i32 {
    if header.list_head == SLOT_NONE {
        return SLOT_NONE;
    }

    let mut hand = header.sieve_hand;
    if hand == SLOT_NONE {
        hand = header.list_head;
    }

    // We may need to scan up to 2× the list length (one pass to clear visited bits,
    // one pass to find an unvisited entry).
    let capacity = header.capacity;
    for _ in 0..capacity * 2 {
        let s = slot_mut(slab_base, slot_size, hand);
        if s.visited != 0 {
            // Second chance: clear visited bit, advance hand
            s.visited = 0;
            let next = s.next;
            hand = if next != SLOT_NONE {
                next
            } else {
                header.list_head
            };
        } else {
            // Found an unvisited entry — evict it
            let next = s.next;
            header.sieve_hand = if next != SLOT_NONE {
                next
            } else {
                header.list_head
            };
            return hand;
        }
    }

    // Fallback: evict whatever the hand is pointing at
    header.sieve_hand = SLOT_NONE;
    hand
}
