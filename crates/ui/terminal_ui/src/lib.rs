//! GPUI compatibility boundary and bounded UI message primitives.
//!
//! GPUI types must remain inside this crate or application view modules once
//! an exact GPUI revision is selected.

use std::collections::VecDeque;
use std::num::NonZeroUsize;

mod order_book;
#[cfg(feature = "gpui")]
mod order_book_view;

pub use order_book::{OrderBookSelection, project_order_book};
#[cfg(feature = "gpui")]
pub use order_book_view::{
    OrderBookColumn, OrderBookColumnVisibility, OrderBookConnectionState, ReadOnlyOrderBookView,
};
pub use tradingplot_market_data::{OrderBookColumnLevel, OrderBookFrame, OrderBookRow};

/// A bounded queue that prevents background producers from growing UI work.
#[derive(Debug)]
pub struct BoundedUiQueue<Message> {
    capacity: NonZeroUsize,
    messages: VecDeque<Message>,
}

impl<Message> BoundedUiQueue<Message> {
    #[must_use]
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            capacity,
            messages: VecDeque::with_capacity(capacity.get()),
        }
    }

    /// Enqueues a message or returns it unchanged when the queue is full.
    ///
    /// # Errors
    ///
    /// Returns the original message when the bounded queue has reached capacity.
    pub fn try_push(&mut self, message: Message) -> Result<(), Message> {
        if self.messages.len() >= self.capacity.get() {
            return Err(message);
        }
        self.messages.push_back(message);
        Ok(())
    }

    /// Drains the bounded batch to be merged by the caller for one UI frame.
    pub fn drain(&mut self) -> impl Iterator<Item = Message> + '_ {
        self.messages.drain(..)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}
