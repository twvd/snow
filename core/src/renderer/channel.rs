use anyhow::Result;

use super::{DisplayBuffer, FrameReceiver};

/// A renderer that feeds it display buffer back over a channel.
pub struct ChannelRenderer {
    displaybuffer: DisplayBuffer,
    channel: FrameReceiver,
}

impl Default for ChannelRenderer {
    fn default() -> Self {
        Self {
            displaybuffer: DisplayBuffer::new(0usize, 0usize),
            channel: Default::default(),
        }
    }
}

impl ChannelRenderer {
    pub fn get_receiver(&self) -> FrameReceiver {
        self.channel.clone()
    }

    /// Gets a reference to the back buffer
    pub fn buffer_mut(&mut self) -> &mut DisplayBuffer {
        &mut self.displaybuffer
    }

    /// False while the last frame has not been picked up yet, so that
    /// rendering a new one can be skipped
    pub fn frame_wanted(&self) -> bool {
        self.channel.lock().unwrap().is_none()
    }

    /// Renders changes to screen
    pub fn update(&mut self) -> Result<()> {
        let new_buffer = self.displaybuffer.new_from_this();
        let buffer = std::mem::replace(&mut self.displaybuffer, new_buffer);
        *self.channel.lock().unwrap() = Some(buffer);
        Ok(())
    }
}
