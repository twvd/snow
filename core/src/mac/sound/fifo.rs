//! Sample FIFO of the sound chips

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// Size of a FIFO in samples
pub const FIFO_SIZE: usize = 0x400;

/// A FIFO of 8-bit samples. When it is empty, the last sample played goes on.
#[derive(Serialize, Deserialize)]
pub struct Fifo {
    samples: VecDeque<u8>,

    /// Last sample played
    last: u8,
}

impl Fifo {
    pub fn new(last: u8) -> Self {
        Self {
            samples: VecDeque::with_capacity(FIFO_SIZE),
            last,
        }
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Queues a sample, unless the FIFO is full. Returns true if the sample
    /// filled it.
    pub fn push(&mut self, sample: u8) -> bool {
        if self.samples.len() == FIFO_SIZE {
            return false;
        }
        self.samples.push_back(sample);
        self.samples.len() == FIFO_SIZE
    }

    /// Plays the next sample, or the last one again if the FIFO is empty
    pub fn pop(&mut self) -> u8 {
        self.last = self.samples.pop_front().unwrap_or(self.last);
        self.last
    }

    /// Last sample played
    pub fn last(&self) -> u8 {
        self.last
    }

    /// Sets the sample to play on when the FIFO is empty
    pub fn set_last(&mut self, sample: u8) {
        self.last = sample;
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }
}
