use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};

use crate::renderer::AUDIO_BUFFER_SIZE;

pub const AUDIO_QUEUE_LEN: usize = 2;

pub type AudioBuffer = Box<[u8]>;

/// Apple Sound Chip
/// TODO currently just stubbed
pub struct Asc {
    sender: Sender<AudioBuffer>,
    pub receiver: Receiver<AudioBuffer>,
    buffer: Vec<u8>,
    silent: bool,
}

impl Default for Asc {
    fn default() -> Self {
        let (sender, receiver) = crossbeam_channel::bounded(AUDIO_QUEUE_LEN);
        Self {
            sender,
            receiver,
            buffer: Vec::with_capacity(AUDIO_BUFFER_SIZE),
            silent: true,
        }
    }
}

impl Asc {
    pub fn push(&mut self, _val: u8) -> Result<()> {
        Ok(())
    }

    pub fn is_silent(&self) -> bool {
        self.silent
    }
}
