//! Event queue: the SAX-like stream filters observe and mutate.
//!
//! Ports `HtmlEvent` and the `HtmlEventList`. The queue is a doubly-linked
//! slab: [`SlotId`] handles stay valid across insertions, removals, and
//! splices, exactly like the C++ `std::list` iterators.

use crate::element::NodeId;

/// Queue slot handle; [`NO_SLOT`] is the end sentinel (`queue_.end()`).
pub type SlotId = u32;
/// End sentinel.
pub const NO_SLOT: SlotId = u32::MAX;

/// One event in the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    StartDocument,
    EndDocument,
    StartElement(NodeId),
    EndElement(NodeId),
    Leaf(NodeId),
}

/// Queue slot with intrusive links.
#[derive(Debug, Clone)]
struct Slot {
    event: Event,
    line_number: u32,
    prev: SlotId,
    next: SlotId,
}

/// Doubly-linked event queue with stable slot handles.
#[derive(Debug)]
pub struct EventQueue {
    slots: Vec<Slot>,
    free: Vec<SlotId>,
    head: SlotId,
    tail: SlotId,
    len: usize,
}

impl Default for EventQueue {
    fn default() -> Self {
        // Empty means `NO_SLOT` links: a derived `Default` would leave
        // `head`/`tail` at 0, aliasing the first allocated slot.
        EventQueue {
            slots: Vec::new(),
            free: Vec::new(),
            head: NO_SLOT,
            tail: NO_SLOT,
            len: 0,
        }
    }
}

impl EventQueue {
    #[must_use]
    pub fn new() -> Self {
        EventQueue::default()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[must_use]
    pub fn head(&self) -> SlotId {
        self.head
    }

    #[must_use]
    pub fn tail(&self) -> SlotId {
        self.tail
    }

    fn alloc(&mut self, event: Event, line_number: u32) -> SlotId {
        if let Some(slot) = self.free.pop() {
            self.slots[slot as usize] = Slot {
                event,
                line_number,
                prev: NO_SLOT,
                next: NO_SLOT,
            };
            slot
        } else {
            let slot = self.slots.len() as SlotId;
            self.slots.push(Slot {
                event,
                line_number,
                prev: NO_SLOT,
                next: NO_SLOT,
            });
            slot
        }
    }

    /// Inserts before `at` (`NO_SLOT` appends at the end); returns the slot.
    pub fn insert_before(&mut self, at: SlotId, event: Event, line_number: u32) -> SlotId {
        let slot = self.alloc(event, line_number);
        if self.head == NO_SLOT {
            // Appending to an empty queue; any `at` means the same slot.
            self.head = slot;
            self.tail = slot;
        } else if at == NO_SLOT {
            let tail = self.tail;
            self.slots[slot as usize].prev = tail;
            self.slots[tail as usize].next = slot;
            self.tail = slot;
        } else {
            let next = at;
            let prev = self.slots[next as usize].prev;
            self.slots[slot as usize].prev = prev;
            self.slots[slot as usize].next = next;
            self.slots[next as usize].prev = slot;
            if prev == NO_SLOT {
                self.head = slot;
            } else {
                self.slots[prev as usize].next = slot;
            }
        }
        self.len += 1;
        slot
    }

    /// Appends at the end; returns the slot.
    pub fn push_back(&mut self, event: Event, line_number: u32) -> SlotId {
        self.insert_before(NO_SLOT, event, line_number)
    }

    /// Prepends at the front; returns the slot.
    pub fn push_front(&mut self, event: Event, line_number: u32) -> SlotId {
        self.insert_before(self.head, event, line_number)
    }

    /// Unlinks a slot without recycling it (delayed/deferred holds).
    /// Returns the slot that followed it.
    pub fn detach(&mut self, slot: SlotId) -> SlotId {
        let prev = self.slots[slot as usize].prev;
        let next = self.slots[slot as usize].next;
        if prev == NO_SLOT {
            self.head = next;
        } else {
            self.slots[prev as usize].next = next;
        }
        if next == NO_SLOT {
            self.tail = prev;
        } else {
            self.slots[next as usize].prev = prev;
        }
        self.slots[slot as usize].prev = NO_SLOT;
        self.slots[slot as usize].next = NO_SLOT;
        self.len -= 1;
        next
    }

    /// Re-links a detached slot before `at` (`NO_SLOT` appends).
    pub fn link_before(&mut self, slot: SlotId, at: SlotId) {
        if self.head == NO_SLOT {
            self.head = slot;
            self.tail = slot;
            self.slots[slot as usize].prev = NO_SLOT;
            self.slots[slot as usize].next = NO_SLOT;
        } else if at == NO_SLOT {
            let tail = self.tail;
            self.slots[slot as usize].prev = tail;
            self.slots[slot as usize].next = NO_SLOT;
            self.slots[tail as usize].next = slot;
            self.tail = slot;
        } else {
            let prev = self.slots[at as usize].prev;
            self.slots[slot as usize].prev = prev;
            self.slots[slot as usize].next = at;
            self.slots[at as usize].prev = slot;
            if prev == NO_SLOT {
                self.head = slot;
            } else {
                self.slots[prev as usize].next = slot;
            }
        }
        self.len += 1;
    }

    /// Removes a slot, returning the slot that followed it.
    pub fn erase(&mut self, slot: SlotId) -> SlotId {
        let next = self.detach(slot);
        self.free.push(slot);
        next
    }

    /// Moves `[first, last)` before `at` (`splice`).
    pub fn splice(&mut self, at: SlotId, first: SlotId, last: SlotId) {
        if first == last || first == at {
            return;
        }
        // Detach [first, last).
        let before = self.slots[first as usize].prev;
        let last_in = if last == NO_SLOT {
            self.tail
        } else {
            self.slots[last as usize].prev
        };
        if before == NO_SLOT {
            self.head = last;
        } else {
            self.slots[before as usize].next = last;
        }
        if last == NO_SLOT {
            self.tail = before;
        } else {
            self.slots[last as usize].prev = before;
        }
        // Attach before `at`.
        if self.head == NO_SLOT {
            self.head = first;
            self.tail = last_in;
            self.slots[first as usize].prev = NO_SLOT;
            self.slots[last_in as usize].next = NO_SLOT;
        } else if at == NO_SLOT {
            let tail = self.tail;
            self.slots[tail as usize].next = first;
            self.slots[first as usize].prev = tail;
            self.slots[last_in as usize].next = NO_SLOT;
            self.tail = last_in;
        } else {
            let prev = self.slots[at as usize].prev;
            self.slots[first as usize].prev = prev;
            self.slots[last_in as usize].next = at;
            self.slots[at as usize].prev = last_in;
            if prev == NO_SLOT {
                self.head = first;
            } else {
                self.slots[prev as usize].next = first;
            }
        }
    }

    /// Clears the queue, recycling all slots.
    pub fn clear(&mut self) {
        let mut slot = self.head;
        while slot != NO_SLOT {
            let next = self.slots[slot as usize].next;
            self.free.push(slot);
            slot = next;
        }
        self.head = NO_SLOT;
        self.tail = NO_SLOT;
        self.len = 0;
    }

    #[must_use]
    pub fn get(&self, slot: SlotId) -> (Event, u32) {
        let entry = &self.slots[slot as usize];
        (entry.event, entry.line_number)
    }

    #[must_use]
    pub fn next(&self, slot: SlotId) -> SlotId {
        if slot == NO_SLOT {
            NO_SLOT
        } else {
            self.slots[slot as usize].next
        }
    }

    #[must_use]
    pub fn prev(&self, slot: SlotId) -> SlotId {
        if slot == NO_SLOT {
            NO_SLOT
        } else {
            self.slots[slot as usize].prev
        }
    }

    /// Collects events front to back (for tests and debugging).
    #[must_use]
    pub fn snapshot(&self) -> Vec<Event> {
        let mut events = Vec::with_capacity(self.len);
        let mut slot = self.head;
        while slot != NO_SLOT {
            events.push(self.slots[slot as usize].event);
            slot = self.slots[slot as usize].next;
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_erase_splice() {
        let mut queue = EventQueue::new();
        let a = queue.push_back(Event::StartDocument, 1);
        let b = queue.push_back(Event::Leaf(7), 2);
        let c = queue.push_back(Event::EndDocument, 3);
        assert_eq!(
            queue.snapshot(),
            [Event::StartDocument, Event::Leaf(7), Event::EndDocument]
        );
        // Handles stay valid across insert/remove (std::list semantics).
        let front = queue.push_front(Event::Leaf(0), 0);
        assert_eq!(queue.get(b), (Event::Leaf(7), 2));
        assert_eq!(queue.erase(b), c);
        assert_eq!(
            queue.snapshot(),
            [Event::Leaf(0), Event::StartDocument, Event::EndDocument]
        );
        // Splice [front, a) to the end.
        queue.splice(NO_SLOT, front, a);
        assert_eq!(
            queue.snapshot(),
            [Event::StartDocument, Event::EndDocument, Event::Leaf(0)]
        );
        queue.clear();
        assert!(queue.is_empty());
    }
}
