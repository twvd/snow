use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::audio_filter::AudioFilter;
use crate::renderer::{default_audio_sink, AudioSink, AUDIO_BUFFER_SIZE, AUDIO_CHANNELS};

#[derive(Serialize, Deserialize)]
pub struct AudioState {
    #[serde(skip, default = "default_audio_sink")]
    sink: Box<dyn AudioSink>,
    buffer: Vec<f32>,
    silent: bool,
    filter: AudioFilter,
}

impl Default for AudioState {
    fn default() -> Self {
        Self {
            sink: default_audio_sink(),
            buffer: Vec::with_capacity(AUDIO_BUFFER_SIZE),
            silent: true,
            filter: AudioFilter::new(),
        }
    }
}

impl AudioState {
    pub fn set_sink(&mut self, sink: Box<dyn AudioSink>) {
        self.sink = sink;
    }

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
            self.sink.send(buffer.into_boxed_slice())?;
        }
        Ok(())
    }

    pub fn is_silent(&self) -> bool {
        self.silent
    }
}
