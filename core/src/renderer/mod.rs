pub mod channel;

use anyhow::Result;
use crossbeam_channel::Receiver;

use std::ops::{Deref, DerefMut};

/// Audio frame channel receiver
pub type AudioReceiver = Receiver<Box<[u8]>>;

/// Amount of samples in the audio buffer
pub const AUDIO_BUFFER_SAMPLES: usize = 512;

/// Audio buffer size (total for all channels)
pub const AUDIO_BUFFER_SIZE: usize = AUDIO_BUFFER_SAMPLES * AUDIO_CHANNELS;

/// Audio channels
pub const AUDIO_CHANNELS: usize = 2;

/// Display buffer for a single frame
/// Always 24-bit color, RRGGBBAA
pub struct DisplayBuffer {
    width: usize,
    height: usize,
    frame: Vec<u8>,
}

impl DisplayBuffer {
    pub fn new(width: impl Into<usize>, height: impl Into<usize>) -> Self {
        let width = Into::<usize>::into(width);
        let height = Into::<usize>::into(height);

        Self {
            width,
            height,
            frame: Self::allocate_buffer(width, height),
        }
    }

    fn allocate_buffer(width: usize, height: usize) -> Vec<u8> {
        let buffer_size = width * height * 4;

        // TODO use uninitialized memory?
        vec![0; buffer_size]
    }

    pub fn new_from_this(&self) -> Self {
        Self {
            width: self.width,
            height: self.height,
            frame: Self::allocate_buffer(self.width, self.height),
        }
    }

    pub fn width(&self) -> u16 {
        self.width as u16
    }

    pub fn height(&self) -> u16 {
        self.height as u16
    }

    pub fn into_inner(self) -> Vec<u8> {
        self.frame
    }

    pub fn set_size(&mut self, width: impl Into<usize>, height: impl Into<usize>) {
        let newwidth = Into::<usize>::into(width);
        let newheight = Into::<usize>::into(height);
        if newwidth != self.width || newheight != self.height {
            self.width = newwidth;
            self.height = newheight;
            self.frame = Self::allocate_buffer(newwidth, newheight);
        }
    }
}

impl Deref for DisplayBuffer {
    type Target = [u8];

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.frame
    }
}

impl DerefMut for DisplayBuffer {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.frame
    }
}

pub trait Renderer {
    /// Creates a new renderer with a screen of the given size
    fn new(width: u16, height: u16) -> Result<Self>
    where
        Self: Renderer + Sized;

    /// Renders changes to screen
    fn update(&mut self) -> Result<()>;

    /// Gets a reference to the back buffer
    fn buffer_mut(&mut self) -> &mut DisplayBuffer;
}

pub struct NullRenderer {
    buffer: DisplayBuffer,
}

impl Renderer for NullRenderer {
    fn new(width: u16, height: u16) -> Result<Self> {
        Ok(Self {
            buffer: DisplayBuffer::new(width, height),
        })
    }

    fn update(&mut self) -> Result<()> {
        Ok(())
    }

    #[inline(always)]
    fn buffer_mut(&mut self) -> &mut DisplayBuffer {
        &mut self.buffer
    }
}
