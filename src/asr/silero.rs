//! Silero VAD v5 (16 kHz) in plain Rust. It runs before whisper so music, silence and noise never reach the model: that
//! is where "[музика]" and phantom words came from. A port of the 30-line definition in the Silero repository
//! (`src/silero_vad/tinygrad_model.py`), so the app needs no ONNX Runtime, and with it no DirectML.dll or VC++ runtime.
//! `matches_onnx_runtime` pins the output to what ONNX Runtime computes for the same weights.

use std::collections::HashMap;
use std::sync::LazyLock;

const RATE: usize = 16000;
const CHUNK: usize = 512;
// Silero v5 wants 64 samples of previous context prepended to each 512-sample frame; without them it quietly misfires.
const CONTEXT: usize = 64;
const FFT_BINS: usize = 129;
const HIDDEN: usize = 128;

static WEIGHTS: &[u8] = include_bytes!("../../resources/silero_vad_16k.safetensors");

struct Conv {
    /// `[out, in, kernel]`, row-major, as stored.
    w: Vec<f32>,
    bias: Vec<f32>,
    out_ch: usize,
    in_ch: usize,
    kernel: usize,
    stride: usize,
    pad: usize,
}

impl Conv {
    /// `x` is `[channels, time]`, row-major; zero padding, as in the original layers. Returns the output and its time length.
    fn forward(&self, x: &[f32], time: usize) -> (Vec<f32>, usize) {
        let out_t = (time + 2 * self.pad - self.kernel) / self.stride + 1;
        let mut out = vec![0f32; self.out_ch * out_t];
        for o in 0..self.out_ch {
            let w_o = &self.w[o * self.in_ch * self.kernel..(o + 1) * self.in_ch * self.kernel];
            let b = self.bias.get(o).copied().unwrap_or(0.0);
            for t in 0..out_t {
                let mut sum = b;
                for i in 0..self.in_ch {
                    let w_i = &w_o[i * self.kernel..(i + 1) * self.kernel];
                    let row = &x[i * time..(i + 1) * time];
                    for (k, weight) in w_i.iter().enumerate() {
                        let at = t * self.stride + k;
                        if at >= self.pad && at - self.pad < time {
                            sum += weight * row[at - self.pad];
                        }
                    }
                }
                out[o * out_t + t] = sum;
            }
        }
        (out, out_t)
    }
}

struct Model {
    stft: Conv,
    convs: [Conv; 4],
    weight_ih: Vec<f32>,
    weight_hh: Vec<f32>,
    bias_ih: Vec<f32>,
    bias_hh: Vec<f32>,
    final_w: Vec<f32>,
    final_b: f32,
}

/// safetensors: 8-byte header length, a JSON header with byte offsets, then little-endian data.
fn load() -> Model {
    let n = u64::from_le_bytes(WEIGHTS[..8].try_into().unwrap()) as usize;
    let header: serde_json::Value = serde_json::from_slice(&WEIGHTS[8..8 + n]).expect("bundled weights header");
    let data = &WEIGHTS[8 + n..];
    let tensors: HashMap<String, Vec<f32>> = header
        .as_object()
        .expect("bundled weights header")
        .iter()
        .filter_map(|(name, v)| {
            let off = v.get("data_offsets")?.as_array()?;
            let (a, b) = (off[0].as_u64()? as usize, off[1].as_u64()? as usize);
            Some((name.clone(), data[a..b].as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect()))
        })
        .collect();
    let get = |name: &str| tensors.get(name).cloned().unwrap_or_else(|| panic!("bundled weights lack {name}"));
    let conv = |name: &str, out_ch, in_ch, stride| Conv { w: get(&format!("{name}.weight")), bias: get(&format!("{name}.bias")), out_ch, in_ch, kernel: 3, stride, pad: 1 };
    Model {
        stft: Conv { w: get("stft_conv.weight"), bias: Vec::new(), out_ch: 2 * FFT_BINS, in_ch: 1, kernel: 256, stride: 128, pad: 0 },
        convs: [conv("conv1", 128, FFT_BINS, 1), conv("conv2", 64, 128, 2), conv("conv3", 64, 64, 2), conv("conv4", 128, 64, 1)],
        weight_ih: get("lstm_cell.weight_ih"),
        weight_hh: get("lstm_cell.weight_hh"),
        bias_ih: get("lstm_cell.bias_ih"),
        bias_hh: get("lstm_cell.bias_hh"),
        final_w: get("final_conv.weight"),
        final_b: get("final_conv.bias")[0],
    }
}

static MODEL: LazyLock<Model> = LazyLock::new(load);

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

struct State {
    h: [f32; HIDDEN],
    c: [f32; HIDDEN],
}

impl Default for State {
    fn default() -> Self {
        Self { h: [0.0; HIDDEN], c: [0.0; HIDDEN] }
    }
}

impl Model {
    /// One frame of `CONTEXT + CHUNK` samples → probability of speech.
    fn step(&self, frame: &[f32], st: &mut State) -> f32 {
        // Reflect-pad the right edge by 64, without repeating the edge sample.
        let n = frame.len();
        let mut x = Vec::with_capacity(n + 64);
        x.extend_from_slice(frame);
        x.extend((0..64).map(|i| frame[n - 2 - i]));
        let (spec, t) = self.stft.forward(&x, x.len());
        let mut x: Vec<f32> = (0..FFT_BINS * t).map(|i| (spec[i] * spec[i] + spec[i + FFT_BINS * t] * spec[i + FFT_BINS * t]).sqrt()).collect();
        let mut t = t;
        for c in &self.convs {
            let (y, ny) = c.forward(&x, t);
            x = y;
            t = ny;
            x.iter_mut().for_each(|v| *v = v.max(0.0));
        }
        // LSTM cell, gates in PyTorch's order: input, forget, cell, output.
        let mut gates = [0f32; 4 * HIDDEN];
        for (g, out) in gates.iter_mut().enumerate() {
            let (wi, wh) = (&self.weight_ih[g * HIDDEN..(g + 1) * HIDDEN], &self.weight_hh[g * HIDDEN..(g + 1) * HIDDEN]);
            *out = self.bias_ih[g] + self.bias_hh[g] + wi.iter().zip(&x).map(|(a, b)| a * b).sum::<f32>() + wh.iter().zip(&st.h).map(|(a, b)| a * b).sum::<f32>();
        }
        for i in 0..HIDDEN {
            let (ig, fg, cg, og) = (sigmoid(gates[i]), sigmoid(gates[HIDDEN + i]), gates[2 * HIDDEN + i].tanh(), sigmoid(gates[3 * HIDDEN + i]));
            st.c[i] = fg * st.c[i] + ig * cg;
            st.h[i] = og * st.c[i].tanh();
        }
        sigmoid(self.final_b + self.final_w.iter().zip(&st.h).map(|(w, h)| w * h.max(0.0)).sum::<f32>())
    }

    fn probabilities(&self, pcm: &[i16]) -> Vec<f32> {
        let mut st = State::default();
        let mut context = [0f32; CONTEXT];
        let mut frame = Vec::with_capacity(CONTEXT + CHUNK);
        pcm.as_chunks::<CHUNK>()
            .0
            .iter()
            .map(|chunk| {
                frame.clear();
                frame.extend_from_slice(&context);
                frame.extend(chunk.iter().map(|&s| s as f32 / 32768.0));
                context.copy_from_slice(&frame[CHUNK..]);
                self.step(&frame, &mut st)
            })
            .collect()
    }
}

/// Seconds of detected speech in the clip.
pub fn speech_seconds(pcm: &[i16], threshold: f32) -> f64 {
    let hits = MODEL.probabilities(pcm).iter().filter(|p| **p >= threshold).count();
    (hits * CHUNK) as f64 / RATE as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Voiced-speech-like harmonics and noise under a syllable envelope, so the model has something non-trivial to chew on.
    fn signal(n: usize) -> Vec<i16> {
        let mut seed = 12345u32;
        (0..n)
            .map(|i| {
                seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
                let noise = ((seed >> 16) & 0x7fff) as f32 / 32768.0 - 0.5;
                let t = i as f32 / 16000.0;
                let tau = std::f32::consts::TAU;
                let voiced = (t * 140.0 * tau).sin() + 0.5 * (t * 420.0 * tau).sin() + 0.25 * (t * 1100.0 * tau).sin();
                let env = ((t * 4.0 * tau).sin() * 0.5 + 0.5).powi(2);
                ((voiced * 0.5 + noise * 0.3) * env * 12000.0) as i16
            })
            .collect()
    }

    /// (frame, probability) as computed by ONNX Runtime 1.2x from silero_vad.onnx v6.2 for `signal`. On real speech the
    /// port was compared frame by frame the same way: no decision changed and the largest difference rounded to 0.00000.
    #[test]
    fn matches_onnx_runtime() {
        let golden = [(0, 0.08754), (12, 0.00890), (24, 0.01213), (36, 0.00728), (48, 0.00452), (60, 0.00100), (72, 0.00279), (84, 0.00214)];
        let p = MODEL.probabilities(&signal(48000));
        assert_eq!(p.len(), 93);
        for (frame, want) in golden {
            assert!((p[frame] - want).abs() < 1e-4, "frame {frame}: {} vs {want}", p[frame]);
        }
    }

    #[test]
    fn silence_and_tone_are_not_speech() {
        let silence = vec![0i16; 16000];
        let tone: Vec<i16> = (0..16000).map(|n| ((n as f32 * 440.0 * std::f32::consts::TAU / 16000.0).sin() * 8000.0) as i16).collect();
        assert!(speech_seconds(&silence, 0.5) < 0.2);
        assert!(speech_seconds(&tone, 0.5) < 0.2);
    }
}
