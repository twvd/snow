use biquad::{Biquad, Coefficients, DirectForm2Transposed, ToHertz, Q_BUTTERWORTH_F32};
use serde::{Deserialize, Serialize};

const SAMPLE_RATE_HZ: f32 = 22254.0;
const HIGHPASS_CUTOFF_HZ: f32 = 10.0;

#[derive(Serialize, Deserialize)]
pub struct AudioFilter {
    #[serde(skip, default = "create_filter")]
    filter_l: DirectForm2Transposed<f32>,
    #[serde(skip, default = "create_filter")]
    filter_r: DirectForm2Transposed<f32>,
}

fn create_filter() -> DirectForm2Transposed<f32> {
    let coeffs = Coefficients::<f32>::from_params(
        biquad::Type::HighPass,
        SAMPLE_RATE_HZ.hz(),
        HIGHPASS_CUTOFF_HZ.hz(),
        Q_BUTTERWORTH_F32,
    )
    .unwrap();
    DirectForm2Transposed::<f32>::new(coeffs)
}

impl Default for AudioFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioFilter {
    pub fn new() -> Self {
        Self {
            filter_l: create_filter(),
            filter_r: create_filter(),
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Filter a mono f32 sample (centered around 0.0)
    pub fn filter_mono(&mut self, sample: f32) -> f32 {
        self.filter_l.run(sample)
    }

    /// Filter a stereo f32 sample pair (centered around 0.0)
    pub fn filter_stereo(&mut self, sample_l: f32, sample_r: f32) -> (f32, f32) {
        let filtered_l = self.filter_l.run(sample_l);
        let filtered_r = self.filter_r.run(sample_r);
        (filtered_l, filtered_r)
    }
}
