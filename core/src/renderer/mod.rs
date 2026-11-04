pub mod channel;

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};

use std::ops::{Deref, DerefMut};

/// Audio frame buffer
pub type AudioBuffer = Box<[f32]>;

/// Audio frame channel receiver
pub type AudioReceiver = Receiver<AudioBuffer>;

/// Audio channel queue length.
/// Higher values add latency to audio, but might fix underruns.
pub const AUDIO_QUEUE_LEN: usize = 4; // TODO: make this configurable?

/// Amount of samples in the audio buffer
pub const AUDIO_BUFFER_SAMPLES: usize = 512;

/// Audio buffer size (total for all channels)
pub const AUDIO_BUFFER_SIZE: usize = AUDIO_BUFFER_SAMPLES * AUDIO_CHANNELS;

/// Audio channels
pub const AUDIO_CHANNELS: usize = 2;

pub trait AudioProvider: Send {
    fn create_stream(
        &mut self,
        freq: i32,
        channels: u8,
        samples: u16,
    ) -> Result<Box<dyn AudioSink>>;
}

/// Audio sink for produced buffers
pub trait AudioSink: Send {
    fn send(&self, buffer: AudioBuffer) -> Result<()>;
    fn is_full(&self) -> bool;
    fn is_empty(&self) -> bool;
}

/// Crossbeam-backed audio sink
pub struct ChannelAudioSink {
    sender: Sender<AudioBuffer>,
    receiver: Receiver<AudioBuffer>,
}

impl ChannelAudioSink {
    pub fn new() -> Self {
        let (sender, receiver) = crossbeam_channel::bounded(AUDIO_QUEUE_LEN);
        Self { sender, receiver }
    }

    pub fn receiver(&self) -> AudioReceiver {
        self.receiver.clone()
    }

    /// Clears/flushes the entire current content in the sink
    pub fn clear(&self) {
        while self.receiver.try_recv().is_ok() {}
    }
}

impl AudioSink for ChannelAudioSink {
    fn send(&self, buffer: AudioBuffer) -> Result<()> {
        self.sender.send(buffer)?;
        Ok(())
    }

    fn is_full(&self) -> bool {
        self.sender.is_full()
    }

    fn is_empty(&self) -> bool {
        self.sender.is_empty()
    }
}

struct NullAudioSink();

impl AudioSink for NullAudioSink {
    fn send(&self, _buffer: AudioBuffer) -> Result<()> {
        Ok(())
    }

    fn is_full(&self) -> bool {
        false
    }

    fn is_empty(&self) -> bool {
        true
    }
}

pub fn null_audio_sink() -> Box<dyn AudioSink> {
    Box::new(NullAudioSink())
}

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

/// The RGBA pixels of a monochrome display, white for 0 and black for 1
pub const MONOCHROME_LUT: [[u8; 4]; 2] = [[0xEE, 0xEE, 0xEE, 0xFF], [0x22, 0x22, 0x22, 0xFF]];

/// Renders packed pixels, most significant first, as RGBA pixels through a
/// lookup table of their N values, so of log2(N) bits each. Fills `out`
/// whole, from the first bytes of `fb`.
#[inline]
pub fn render_packed<const N: usize>(fb: &[u8], out: &mut [u8], lut: &[[u8; 4]; N]) {
    let bits = N.trailing_zeros() as usize;
    let per_byte = 8 / bits;
    let expand = |chunk: &mut [u8], byte: u8| {
        for (i, px) in chunk.chunks_exact_mut(4).enumerate() {
            let shift = 8 - bits * (i + 1);
            // Masked to the table size, which takes the bounds check away
            px.copy_from_slice(&lut[usize::from(byte >> shift) & (N - 1)]);
        }
    };

    let fb = &fb[..(out.len() / 4).div_ceil(per_byte)];
    let mut chunks = out.chunks_exact_mut(per_byte * 4);
    for (chunk, &byte) in (&mut chunks).zip(fb) {
        expand(chunk, byte);
    }
    // Pixels in a last, partly used byte
    let rest = chunks.into_remainder();
    if !rest.is_empty() {
        expand(rest, fb[fb.len() - 1]);
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

    /// False while the last frame has not been picked up yet, so that
    /// rendering a new one can be skipped
    fn frame_wanted(&self) -> bool {
        true
    }
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
