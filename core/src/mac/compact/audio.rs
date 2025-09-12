use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};

use crate::renderer::{AUDIO_BUFFER_SIZE, AUDIO_CHANNELS};

pub const AUDIO_QUEUE_LEN: usize = 2;

pub type AudioBuffer = Box<[u8]>;

#[derive(Serialize, Deserialize)]
pub struct AudioState {
    #[serde(skip)]
    sender: Option<Sender<AudioBuffer>>,
    #[serde(skip)]
    pub receiver: Option<Receiver<AudioBuffer>>,
    buffer: Vec<u8>,
    silent: bool,
}

impl Default for AudioState {
    fn default() -> Self {
        let (sender, receiver) = crossbeam_channel::bounded(AUDIO_QUEUE_LEN);
        Self {
            sender: Some(sender),
            receiver: Some(receiver),
            buffer: Vec::with_capacity(AUDIO_BUFFER_SIZE),
            silent: true,
        }
    }
}

impl AudioState {
    pub fn push(&mut self, val: u8) -> Result<()> {
        if val != 0 && val != 0xFF {
            self.silent = false;
        }

        for _ in 0..AUDIO_CHANNELS {
            self.buffer.push(val);
        }
        if self.buffer.len() >= AUDIO_BUFFER_SIZE {
            let buffer = std::mem::replace(&mut self.buffer, Vec::with_capacity(AUDIO_BUFFER_SIZE));
            self.silent = buffer.iter().all(|&s| s == buffer[0]);
            self.sender
                .as_ref()
                .unwrap()
                .send(buffer.into_boxed_slice())?;
        }
        Ok(())
    }

    pub fn is_silent(&self) -> bool {
        self.silent
    }
}
