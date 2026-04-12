use std::sync::{Arc, Mutex};

use anyhow::Result;

use super::{DisplayBuffer, Renderer};

/// A renderer that feeds it display buffer back over a channel.
pub struct ChannelRenderer {
    displaybuffer: DisplayBuffer,
    channel: Arc<Mutex<Option<DisplayBuffer>>>,
}

impl ChannelRenderer {
    pub fn get_receiver(&self) -> Arc<Mutex<Option<DisplayBuffer>>> {
        self.channel.clone()
    }
}

impl Renderer for ChannelRenderer {
    /// Creates a new renderer with a screen of the given size
    fn new(width: u16, height: u16) -> Result<Self> {
        Ok(Self {
            displaybuffer: DisplayBuffer::new(width, height),
            channel: Default::default(),
        })
    }

    fn buffer_mut(&mut self) -> &mut DisplayBuffer {
        &mut self.displaybuffer
    }

    /// Renders changes to screen
    fn update(&mut self) -> Result<()> {
        let new_buffer = self.displaybuffer.new_from_this();
        let buffer = std::mem::replace(&mut self.displaybuffer, new_buffer);
        *self.channel.lock().unwrap() = Some(buffer);
        Ok(())
    }
}
