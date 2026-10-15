//! Audio output of the sound chips

use crate::audio_filter::AudioFilter;
use crate::renderer::{AUDIO_BUFFER_SIZE, AudioSink, null_audio_sink};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Takes 8-bit stereo samples to the audio sink, through the audio filter
#[derive(Serialize, Deserialize)]
pub struct AudioPath {
    #[serde(skip, default = "null_audio_sink")]
    sink: Box<dyn AudioSink>,

    buffer: Vec<f32>,
    silent: bool,
    filter: AudioFilter,
}

impl Default for AudioPath {
    fn default() -> Self {
        Self {
            sink: null_audio_sink(),
            buffer: Vec::with_capacity(AUDIO_BUFFER_SIZE),
            silent: true,
            filter: AudioFilter::new(),
        }
    }
}

impl AudioPath {
    pub fn reset(&mut self) {
        self.filter.reset();
    }

    pub fn is_silent(&self) -> bool {
        self.silent
    }

    pub fn push(&mut self, l: u8, r: u8) -> Result<()> {
        if (l, r) != (0, 0) && (l, r) != (0xFF, 0xFF) {
            self.silent = false;
        }

        // Convert u8 (0-255) to f32 in standard audio range (-1.0 to 1.0)
        let sample_l = ((l as f32) - 128.0) / 128.0;
        let sample_r = ((r as f32) - 128.0) / 128.0;

        // Apply filters
        let (filtered_l, filtered_r) = self.filter.filter_stereo(sample_l, sample_r);

        self.buffer.push(filtered_l);
        self.buffer.push(filtered_r);
        // Assuming we're always aligned to 2 here
        if self.buffer.len() >= AUDIO_BUFFER_SIZE {
            let buffer = std::mem::replace(&mut self.buffer, Vec::with_capacity(AUDIO_BUFFER_SIZE));
            self.silent = buffer.iter().all(|&s| s.abs() < 0.01);
            self.sink.send(buffer.into_boxed_slice())?;
        }
        Ok(())
    }

    pub fn set_sink(&mut self, sink: Box<dyn AudioSink>) {
        self.sink = sink;
    }

    pub fn after_deserialize(&mut self) {
        self.set_sink(null_audio_sink());
    }
}
