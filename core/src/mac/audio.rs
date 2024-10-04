use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};

pub const AUDIO_BUFFER_SIZE: usize = 500;
pub const AUDIO_QUEUE_LEN: usize = 2;

pub type AudioBuffer = Box<[u8]>;
pub type AudioReceiver = Receiver<Box<[u8]>>;

pub struct AudioState {
    sender: Sender<AudioBuffer>,
    pub receiver: Receiver<AudioBuffer>,
    buffer: Vec<u8>,
}

impl Default for AudioState {
    fn default() -> Self {
        let (sender, receiver) = crossbeam_channel::bounded(AUDIO_QUEUE_LEN);
        Self {
            sender,
            receiver,
            buffer: Vec::with_capacity(AUDIO_BUFFER_SIZE),
        }
    }
}

impl AudioState {
    pub fn push(&mut self, val: u8) -> Result<()> {
        self.buffer.push(val);
        if self.buffer.len() >= AUDIO_BUFFER_SIZE {
            let buffer = std::mem::replace(&mut self.buffer, Vec::with_capacity(AUDIO_BUFFER_SIZE));
            self.sender.send(buffer.into_boxed_slice())?;
        }
        Ok(())
    }
}
