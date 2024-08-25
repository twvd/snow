pub mod channel;
pub mod sdl;

use anyhow::Result;

use std::iter;
use std::sync::atomic::AtomicU8;
use std::sync::Arc;

/// Thread-safe display buffer
pub type DisplayBuffer = Arc<Vec<AtomicU8>>;

fn new_displaybuffer(width: usize, height: usize) -> DisplayBuffer {
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

pub struct NullRenderer {}
impl Renderer for NullRenderer {
    fn new(_width: usize, _height: usize) -> Result<Self> {
        Ok(Self {})
    }

    fn update(&mut self) -> Result<()> {
        Ok(())
    }

    fn get_buffer(&mut self) -> DisplayBuffer {
        unreachable!()
    }
}
