use std::sync::{Arc, Mutex};

use anyhow::Result;

use super::{DisplayBuffer, Renderer};

type NotifyFn = Box<dyn Fn() + Send>;

/// Tells the frontend, from the emulator's thread, each time a new frame is
/// ready to take, so it can draw it right away rather than at its next redraw
#[derive(Clone, Default)]
pub struct FrameNotify(Arc<Mutex<Option<NotifyFn>>>);

impl FrameNotify {
    /// Calls `f` for each new frame from now on
    pub fn set(&self, f: impl Fn() + Send + 'static) {
        *self.0.lock().unwrap() = Some(Box::new(f));
    }

    fn notify(&self) {
        if let Some(f) = self.0.lock().unwrap().as_ref() {
            f();
        }
    }
}

/// A renderer that feeds it display buffer back over a channel.
pub struct ChannelRenderer {
    displaybuffer: DisplayBuffer,
    channel: Arc<Mutex<Option<DisplayBuffer>>>,
    notify: FrameNotify,
}

impl ChannelRenderer {
    pub fn get_receiver(&self) -> Arc<Mutex<Option<DisplayBuffer>>> {
        self.channel.clone()
    }

    pub fn frame_notify(&self) -> FrameNotify {
        self.notify.clone()
    }
}

impl Renderer for ChannelRenderer {
    /// Creates a new renderer with a screen of the given size
    fn new(width: u16, height: u16) -> Result<Self> {
        Ok(Self {
            displaybuffer: DisplayBuffer::new(width, height),
            channel: Default::default(),
            notify: FrameNotify::default(),
        })
    }

    fn buffer_mut(&mut self) -> &mut DisplayBuffer {
        &mut self.displaybuffer
    }

    fn frame_wanted(&self) -> bool {
        self.channel.lock().unwrap().is_none()
    }

    /// Renders changes to screen
    fn update(&mut self) -> Result<()> {
        let new_buffer = self.displaybuffer.new_from_this();
        let mut buffer = std::mem::replace(&mut self.displaybuffer, new_buffer);
        buffer.set_rendered();
        *self.channel.lock().unwrap() = Some(buffer);
        self.notify.notify();
        Ok(())
    }
}
