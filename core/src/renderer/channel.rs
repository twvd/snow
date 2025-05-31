use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, TrySendError};

use super::{DisplayBuffer, Renderer};

/// A renderer that feeds it display buffer back over a channel.
pub struct ChannelRenderer {
    displaybuffer: DisplayBuffer,
    sender: Sender<DisplayBuffer>,
    receiver: Receiver<DisplayBuffer>,
}

impl ChannelRenderer {
    pub fn get_receiver(&self) -> Receiver<DisplayBuffer> {
        self.receiver.clone()
    }
}

impl Renderer for ChannelRenderer {
    /// Creates a new renderer with a screen of the given size
    fn new(width: u16, height: u16) -> Result<Self> {
        let (sender, receiver) = crossbeam_channel::bounded(1);
        Ok(Self {
            displaybuffer: DisplayBuffer::new(width, height),
            sender,
            receiver,
        })
    }

    fn buffer_mut(&mut self) -> &mut DisplayBuffer {
        &mut self.displaybuffer
    }

    /// Renders changes to screen
    fn update(&mut self) -> Result<()> {
        let new_buffer = self.displaybuffer.new_from_this();
        let buffer = std::mem::replace(&mut self.displaybuffer, new_buffer);

        match self.sender.try_send(buffer) {
            Err(TrySendError::Full(_)) => Ok(()),
            e => Ok(e?),
        }
    }
}
