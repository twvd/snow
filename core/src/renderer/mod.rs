pub mod channel;

use anyhow::Result;

use std::iter;
use std::sync::atomic::AtomicU8;
use std::sync::Arc;

/// Thread-safe display buffer
pub type DisplayBuffer = Arc<Vec<AtomicU8>>;

pub fn new_displaybuffer(width: usize, height: usize) -> DisplayBuffer {
    Arc::new(Vec::from_iter(
        iter::repeat_with(|| AtomicU8::new(0)).take(width * height * 4),
    ))
}

pub trait Renderer {
    /// Creates a new renderer with a screen of the given size
    fn new(width: usize, height: usize) -> Result<Self>
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
    fn new(width: usize, height: usize) -> Result<Self> {
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
