use std::sync::Arc;

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, TrySendError};

use super::{new_displaybuffer, DisplayBuffer, Renderer};

/// A renderer that feeds it display buffer back over a channel.
pub struct ChannelRenderer {
    displaybuffer: DisplayBuffer,
    sender: Sender<DisplayBuffer>,
    receiver: Receiver<DisplayBuffer>,
}

impl ChannelRenderer {
    pub fn get_receiver(&mut self) -> Receiver<DisplayBuffer> {
        self.receiver.clone()
    }
}

impl Renderer for ChannelRenderer {
    /// Creates a new renderer with a screen of the given size
    fn new(width: usize, height: usize) -> Result<Self> {
        let (sender, receiver) = crossbeam_channel::bounded(1);
        Ok(Self {
            displaybuffer: new_displaybuffer(width, height),
            sender,
            receiver,
        })
    }

    fn get_buffer(&mut self) -> DisplayBuffer {
        Arc::clone(&self.displaybuffer)
    }

    /// Renders changes to screen
    fn update(&mut self) -> Result<()> {
        // Copy the current buffer as fresh backbuffer so it is possible to
        // do partial updates of the previous frame.
        let new_buffer = self.displaybuffer.clone();
        let buffer = std::mem::replace(&mut self.displaybuffer, new_buffer);

        match self.sender.try_send(buffer) {
            Err(TrySendError::Full(_)) => Ok(()),
            e => Ok(e?),
        }
    }
}
