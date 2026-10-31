use std::sync::Mutex;

use ort::environment::Environment;
use ort::session::Session;
use ort::value::Tensor;

const RATE: i64 = 16000;
const CHUNK: usize = 512;
// Silero v5 wants 64 samples of previous context prepended to each 512-sample frame; without them it quietly misfires.
const CONTEXT: usize = 64;

static MODEL: &[u8] = include_bytes!("../../resources/silero_vad.onnx");

/// Silero VAD speech gate. It runs before whisper so music/silence/noise never reaches the model — that is where
/// "[музика]" and phantom words came from. It loads lazily and, if the runtime fails, degrades to a pass-through
/// (`speech_seconds` → None) so ASR never breaks.
#[derive(Default)]
pub struct SileroGate {
    session: Mutex<Option<Result<(Environment, Session), ()>>>,
}

impl SileroGate {
    fn load() -> Result<(Environment, Session), ()> {
        let r = (|| -> Result<(Environment, Session), String> {
            let env = ort::init().build().map_err(|e| e.to_string())?;
            let b = Session::builder(&env).map_err(|e| e.to_string())?;
            let mut b = b.with_intra_threads(1).map_err(|e| e.to_string())?.with_inter_threads(1).map_err(|e| e.to_string())?;
            let s = b.commit_from_memory(MODEL).map_err(|e| e.to_string())?;
            Ok((env, s))
        })();
        r.map_err(|e| crate::db::diag("silero_unavailable", serde_json::json!({ "message": e })))
    }

    /// Seconds of detected speech in the clip, or None when the gate is unavailable (the caller then lets it through).
    pub fn speech_seconds(&self, pcm: &[i16], threshold: f32) -> Option<f64> {
        let mut guard = self.session.lock().unwrap();
        let (_, session) = guard.get_or_insert_with(Self::load).as_mut().ok()?;
        match run(session, pcm, threshold) {
            Ok(s) => Some(s),
            Err(e) => {
                crate::db::diag("silero_run_error", serde_json::json!({ "message": e }));
                *guard = Some(Err(()));
                None
            }
        }
    }
}

fn run(session: &mut Session, pcm: &[i16], threshold: f32) -> Result<f64, String> {
    let names: Vec<String> = session.inputs().iter().map(|o| o.name().to_string()).collect();
    let outs: Vec<String> = session.outputs().iter().map(|o| o.name().to_string()).collect();
    let (input_n, state_n, sr_n) = (names.first().cloned().unwrap_or("input".into()), names.get(1).cloned().unwrap_or("state".into()), names.get(2).cloned().unwrap_or("sr".into()));
    let (out_n, state_out_n) = (outs.first().cloned().unwrap_or("output".into()), outs.get(1).cloned().unwrap_or("stateN".into()));
    let mut state = vec![0f32; 2 * 128];
    let mut context = [0f32; CONTEXT];
    let mut hits = 0usize;
    let mut i = 0;
    while i + CHUNK <= pcm.len() {
        let mut frame = Vec::with_capacity(CONTEXT + CHUNK);
        frame.extend_from_slice(&context);
        frame.extend(pcm[i..i + CHUNK].iter().map(|&s| s as f32 / 32768.0));
        context.copy_from_slice(&frame[CHUNK..]);
        let input = Tensor::from_array(([1usize, CONTEXT + CHUNK], frame)).map_err(|e| e.to_string())?;
        let st = Tensor::from_array(([2usize, 1, 128], state.clone())).map_err(|e| e.to_string())?;
        let sr = Tensor::from_array(((), vec![RATE])).map_err(|e| e.to_string())?;
        let outputs = session.run(ort::inputs![input_n.as_str() => input, state_n.as_str() => st, sr_n.as_str() => sr]).map_err(|e| e.to_string())?;
        let (_, prob) = outputs[out_n.as_str()].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
        if prob.first().copied().unwrap_or(0.0) >= threshold {
            hits += 1;
        }
        let (_, ns) = outputs[state_out_n.as_str()].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
        state = ns.to_vec();
        i += CHUNK;
    }
    Ok((hits * CHUNK) as f64 / RATE as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_and_tone_are_not_speech() {
        let g = SileroGate::default();
        let silence = vec![0i16; 16000];
        let tone: Vec<i16> = (0..16000).map(|n| ((n as f32 * 440.0 * std::f32::consts::TAU / 16000.0).sin() * 8000.0) as i16).collect();
        let s = g.speech_seconds(&silence, 0.5).expect("silero loads");
        assert!(s < 0.2, "silence {s}");
        assert!(g.speech_seconds(&tone, 0.5).unwrap() < 0.2);
    }
}
