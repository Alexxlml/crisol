//! The daemon's stream of events, numbered.
//!
//! Every event gets the next number as it is sent, under one lock, so the numbers follow the
//! order in which subscribers receive them, whichever thread sends (the actor or the worker).
//! A `status` answer carries the number of the last event sent before its picture was taken
//! (review of phase 12, finding 1): the answer and the events reach a connection by different
//! paths and either may arrive first, so a client keeps only the events numbered after it. A
//! skipped number tells a client that it lost events and should ask for the status again.

use std::sync::{Arc, Mutex, PoisonError};

use crisol_protocol::{Event, Numbered};
use tokio::sync::broadcast;

/// Where the daemon's events go, numbered. Cloning it gives another handle to the same stream.
#[derive(Debug, Clone)]
pub struct Bus {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    sender: broadcast::Sender<Numbered>,
    /// The number of the last event sent; 0 before the first.
    last: Mutex<u64>,
}

impl Bus {
    /// A stream that keeps up to `capacity` events for a subscriber that is behind.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self {
            inner: Arc::new(Inner {
                sender,
                last: Mutex::new(0),
            }),
        }
    }

    /// Numbers `event` and sends it to every subscriber.
    pub fn send(&self, event: Event) {
        let mut last = self
            .inner
            .last
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *last += 1;
        // Nobody subscribed is not an error: the event is simply not wanted.
        let _ = self.inner.sender.send(Numbered {
            event,
            seq: Some(*last),
        });
    }

    /// The number of the last event sent; 0 before the first.
    #[must_use]
    pub fn last(&self) -> u64 {
        *self
            .inner
            .last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Every event sent from now on.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Numbered> {
        self.inner.sender.subscribe()
    }
}

#[cfg(test)]
mod tests {
    // Test code: a failed `unwrap` is the failure report.
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn events_are_numbered_from_one_in_the_order_they_are_sent() {
        let bus = Bus::new(8);
        assert_eq!(bus.last(), 0);
        let mut seen = bus.subscribe();
        bus.send(Event::ReloadStarted);
        bus.clone().send(Event::Reloaded);
        assert_eq!(bus.last(), 2);
        let first = seen.try_recv().unwrap();
        let second = seen.try_recv().unwrap();
        assert_eq!((first.event, first.seq), (Event::ReloadStarted, Some(1)));
        assert_eq!((second.event, second.seq), (Event::Reloaded, Some(2)));
    }

    #[test]
    fn with_nobody_listening_the_numbers_still_go_up() {
        let bus = Bus::new(8);
        bus.send(Event::Reloaded);
        let mut late = bus.subscribe();
        bus.send(Event::Reloaded);
        assert_eq!(late.try_recv().unwrap().seq, Some(2));
    }
}
