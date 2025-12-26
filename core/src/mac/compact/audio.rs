use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};

use crate::audio_filter::AudioFilter;
use crate::renderer::{AUDIO_BUFFER_SIZE, AUDIO_CHANNELS};

pub const AUDIO_QUEUE_LEN: usize = 2;

pub type AudioBuffer = Box<[f32]>;

#[derive(Serialize, Deserialize)]
pub struct AudioState {
    #[serde(skip)]
    sender: Option<Sender<AudioBuffer>>,
    #[serde(skip)]
    pub receiver: Option<Receiver<AudioBuffer>>,
    buffer: Vec<f32>,
    silent: bool,
    filter: AudioFilter,
}

impl Default for AudioState {
    fn default() -> Self {
        let (sender, receiver) = crossbeam_channel::bounded(AUDIO_QUEUE_LEN);
        Self {
            sender: Some(sender),
            receiver: Some(receiver),
            buffer: Vec::with_capacity(AUDIO_BUFFER_SIZE),
            silent: true,
            filter: AudioFilter::new(),
        }
    }
}

impl AudioState {
    pub fn push(&mut self, val: u8) -> Result<()> {
        if val != 0 && val != 0xFF {
            self.silent = false;
        }

        // Convert u8 (0-255) to f32 in standard audio range (-1.0 to 1.0)
        let sample = ((val as f32) - 128.0) / 128.0;

        // Apply filters
        let filtered = self.filter.filter_mono(sample);

        for _ in 0..AUDIO_CHANNELS {
            self.buffer.push(filtered);
        }
        if self.buffer.len() >= AUDIO_BUFFER_SIZE {
            let buffer = std::mem::replace(&mut self.buffer, Vec::with_capacity(AUDIO_BUFFER_SIZE));
            self.silent = buffer.iter().all(|&s| s.abs() < 0.01);
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
