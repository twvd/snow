pub mod channel;

use anyhow::Result;
use crossbeam_channel::Receiver;

use std::iter;
use std::sync::atomic::AtomicU8;
use std::sync::Arc;

/// Audio frame channel receiver
pub type AudioReceiver = Receiver<Box<[u8]>>;

/// Audio buffer size
/// TODO make this model-specific?
pub const AUDIO_BUFFER_SIZE: usize = 500;

/// Thread-safe display buffer
pub type DisplayBuffer = Arc<Vec<AtomicU8>>;

pub fn new_displaybuffer(width: u16, height: u16) -> DisplayBuffer {
    Arc::new(Vec::from_iter(
        iter::repeat_with(|| AtomicU8::new(0)).take(usize::from(width) * usize::from(height) * 4),
    ))
}

pub trait Renderer {
    /// Creates a new renderer with a screen of the given size
    fn new(width: u16, height: u16) -> Result<Self>
    where
        Self: Renderer + Sized;

    /// Renders changes to screen
    fn update(&mut self) -> Result<()>;

    /// Gets a reference to the (lockable) back buffer
    fn get_buffer(&mut self) -> DisplayBuffer;
}

pub struct NullRenderer {
    buffer: DisplayBuffer,
}

impl Renderer for NullRenderer {
    fn new(width: u16, height: u16) -> Result<Self> {
        Ok(Self {
            buffer: new_displaybuffer(width, height),
        })
    }

    fn update(&mut self) -> Result<()> {
        Ok(())
    }

    fn get_buffer(&mut self) -> DisplayBuffer {
        self.buffer.clone()
    }
}
