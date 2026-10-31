//! Audio capture: microphone ("user") and WASAPI loopback of the default output ("remote"), each on its own thread.
//! Samples are downmixed, resampled to 16 kHz mono and cut into utterances by an energy VAD.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::i18n::t;
use crate::types::{now_ms, ComponentState, Source};

const RATE: u32 = 16_000;
const FRAME: usize = 320; // 20 ms

pub struct VadConfig {
    pub start_frames: u32,
    pub hangover_frames: u32,
    pub preroll_frames: usize,
    pub max_frames: usize,
    pub partial_every_frames: u32,
    pub min_speech_frames: u32,
}

pub const VAD: VadConfig = VadConfig { start_frames: 3, hangover_frames: 35, preroll_frames: 15, max_frames: 1000, partial_every_frames: 50, min_speech_frames: 15 };

pub struct Utterance {
    pub id: String,
    pub start_at: i64,
    pub pcm: Vec<i16>,
}

pub enum VadOut {
    Partial(Utterance),
    Final(Utterance),
}

/// Energy VAD with an adaptive noise floor: cheap, deterministic, good enough to cut turns for whisper.
pub struct Vad {
    noise: f32,
    run: u32,
    silence: u32,
    speech: u32,
    frames: Vec<Vec<f32>>,
    preroll: std::collections::VecDeque<Vec<f32>>,
    active: bool,
    since_start: u32,
    start_at: i64,
    id: String,
}

impl Default for Vad {
    fn default() -> Self {
        Self { noise: 0.003, run: 0, silence: 0, speech: 0, frames: Vec::new(), preroll: Default::default(), active: false, since_start: 0, start_at: 0, id: String::new() }
    }
}

impl Vad {
    pub fn push(&mut self, frame: &[f32], now: i64, out: &mut Vec<VadOut>) -> f32 {
        let rms = (frame.iter().map(|v| v * v).sum::<f32>() / frame.len() as f32).sqrt();
        if !self.active {
            self.noise = if rms < self.noise { self.noise * 0.9 + rms * 0.1 } else { self.noise * 0.998 + rms * 0.002 };
        }
        let start_thr = (self.noise * 3.0).max(0.008);
        let keep_thr = (self.noise * 2.0).max(0.005);
        if !self.active {
            self.preroll.push_back(frame.to_vec());
            if self.preroll.len() > VAD.preroll_frames {
                self.preroll.pop_front();
            }
            self.run = if rms > start_thr { self.run + 1 } else { 0 };
            if self.run >= VAD.start_frames {
                self.active = true;
                self.frames = self.preroll.drain(..).collect();
                self.speech = self.run;
                self.silence = 0;
                self.since_start = 0;
                self.start_at = now - self.frames.len() as i64 * 20;
                self.id = crate::types::new_id();
            }
            return rms;
        }
        self.frames.push(frame.to_vec());
        self.since_start += 1;
        if rms > keep_thr {
            self.speech += 1;
            self.silence = 0;
        } else {
            self.silence += 1;
        }
        if self.silence >= VAD.hangover_frames || self.frames.len() >= VAD.max_frames {
            let trim = self.silence >= VAD.hangover_frames;
            self.finish(trim, out);
        } else if self.since_start.is_multiple_of(VAD.partial_every_frames) && self.speech >= VAD.min_speech_frames {
            out.push(VadOut::Partial(Utterance { id: self.id.clone(), start_at: self.start_at, pcm: to_i16(&self.frames) }));
        }
        rms
    }

    /// Flushed on pause/stop so the last words are not lost.
    pub fn flush(&mut self, out: &mut Vec<VadOut>) {
        if self.active {
            self.finish(false, out);
        }
    }

    fn finish(&mut self, trim_tail: bool, out: &mut Vec<VadOut>) {
        let keep = if trim_tail { self.frames.len().saturating_sub((VAD.hangover_frames as f32 * 0.6) as usize) } else { self.frames.len() };
        let u = Utterance { id: self.id.clone(), start_at: self.start_at, pcm: to_i16(&self.frames[..keep]) };
        let enough = self.speech >= VAD.min_speech_frames;
        self.active = false;
        self.frames.clear();
        self.run = 0;
        self.speech = 0;
        self.silence = 0;
        if enough {
            out.push(VadOut::Final(u));
        }
    }
}

fn to_i16(frames: &[Vec<f32>]) -> Vec<i16> {
    frames.iter().flatten().map(|v| (v * 32767.0).round().clamp(-32768.0, 32767.0) as i16).collect()
}

/// Windowed-sinc low-pass (≈7 kHz) followed by fractional decimation to 16 kHz.
pub struct Resampler {
    step: f64,
    taps: Vec<f32>,
    hist: Vec<f32>,
    pos: f64,
}

impl Resampler {
    pub fn new(input_rate: u32) -> Self {
        let n = 63;
        let fc = (7000.0 / input_rate as f64).min(0.45);
        let taps: Vec<f32> = (0..n)
            .map(|i| {
                let m = i as f64 - (n - 1) as f64 / 2.0;
                let sinc = if m == 0.0 { 2.0 * fc } else { (2.0 * std::f64::consts::PI * fc * m).sin() / (std::f64::consts::PI * m) };
                let w = 0.42 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos() + 0.08 * (4.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos();
                (sinc * w) as f32
            })
            .collect();
        let sum: f32 = taps.iter().sum();
        Self { step: input_rate as f64 / RATE as f64, taps: taps.iter().map(|t| t / sum).collect(), hist: vec![0.0; n - 1], pos: 0.0 }
    }

    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let n = self.taps.len();
        self.hist.extend_from_slice(input);
        let filt = |buf: &[f32], at: usize| -> f32 { self.taps.iter().zip(&buf[at..at + n]).map(|(a, b)| a * b).sum() };
        // Filtered sample k sits at hist[k..k+n]; available k: 0..=hist.len()-n.
        let avail = self.hist.len() - (n - 1);
        while self.pos + 1.0 < avail as f64 {
            let k = self.pos.floor() as usize;
            let frac = (self.pos - k as f64) as f32;
            let a = filt(&self.hist, k);
            let b = filt(&self.hist, k + 1);
            out.push(a + (b - a) * frac);
            self.pos += self.step;
        }
        let consumed = (self.pos.floor() as usize).min(avail);
        self.hist.drain(..consumed);
        self.pos -= consumed as f64;
    }
}

pub struct AudioChunk {
    pub session_id: String,
    pub utterance_id: String,
    pub source: Source,
    pub pcm: Vec<i16>,
    pub start_at: i64,
    pub end_at: i64,
    pub is_final: bool,
}

pub trait CaptureSink: Send + Sync {
    fn audio(&self, c: AudioChunk);
    fn level(&self, source: Source, rms: f32);
    fn status(&self, source: Source, state: ComponentState, label: String);
}

#[derive(Clone)]
pub struct CaptureOptions {
    pub session_id: String,
    pub mic_device_id: Option<String>,
    pub capture_mic: bool,
    pub capture_system: bool,
}

pub struct InputDevice {
    pub id: String,
    pub name: String,
}

pub fn input_devices() -> Vec<InputDevice> {
    let host = cpal::default_host();
    host.input_devices()
        .map(|it| it.filter_map(|d| Some(InputDevice { id: d.id().ok()?.to_string(), name: d.to_string() })).collect())
        .unwrap_or_default()
}

fn find_input(id: Option<&str>) -> Option<cpal::Device> {
    let host = cpal::default_host();
    if let Some(id) = id {
        if let Some(d) = host.input_devices().ok()?.find(|d| d.id().map(|x| x.to_string()).as_deref() == Ok(id)) {
            return Some(d);
        }
    }
    host.default_input_device()
}

fn device_for(source: Source, mic: Option<&str>) -> Option<cpal::Device> {
    match source {
        Source::User => find_input(mic),
        Source::Remote => cpal::default_host().default_output_device(),
    }
}

fn device_key(source: Source, mic: Option<&str>) -> Option<String> {
    device_for(source, mic).and_then(|d| d.id().ok()).map(|i| i.to_string())
}

/// Opens a 32-bit float stream on the device (input, or loopback for an output device) and forwards mono samples.
fn open_stream(device: &cpal::Device, source: Source, tx: std::sync::mpsc::Sender<Vec<f32>>, broken: Arc<AtomicBool>) -> Result<(cpal::Stream, u32), String> {
    let supported = match source {
        Source::User => device.default_input_config(),
        Source::Remote => device.default_output_config(),
    }
    .map_err(|e| e.to_string())?;
    let rate = supported.sample_rate();
    let ch = supported.channels().max(1) as usize;
    let config = supported.config();
    let err = move |_e: cpal::Error| broken.store(true, Ordering::SeqCst);
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream::<f32, _, _>(config, move |d, _| { let _ = tx.send(mono(d, ch, |s| s)); }, err, None),
        cpal::SampleFormat::I16 => device.build_input_stream::<i16, _, _>(config, move |d, _| { let _ = tx.send(mono(d, ch, |s| s as f32 / 32768.0)); }, err, None),
        cpal::SampleFormat::I32 => device.build_input_stream::<i32, _, _>(config, move |d, _| { let _ = tx.send(mono(d, ch, |s| s as f32 / 2147483648.0)); }, err, None),
        f => return Err(format!("sample format {f:?}")),
    }
    .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok((stream, rate))
}

fn mono<T: Copy>(data: &[T], ch: usize, conv: impl Fn(T) -> f32) -> Vec<f32> {
    data.chunks(ch).map(|f| f.iter().map(|s| conv(*s)).sum::<f32>() / ch as f32).collect()
}

struct Channel {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

#[derive(Default)]
pub struct CaptureController {
    channels: Mutex<Vec<Channel>>,
    opts: Mutex<Option<CaptureOptions>>,
}

impl CaptureController {
    pub fn start(&self, opts: CaptureOptions, sink: Arc<dyn CaptureSink>) {
        self.stop_threads();
        *self.opts.lock().unwrap() = Some(opts.clone());
        let mut chans = self.channels.lock().unwrap();
        for (source, on) in [(Source::User, opts.capture_mic), (Source::Remote, opts.capture_system)] {
            if !on {
                sink.status(source, ComponentState::Off, t("вимкнено в налаштуваннях", "disabled in settings").into());
                continue;
            }
            let stop = Arc::new(AtomicBool::new(false));
            let (s, o, k) = (stop.clone(), opts.clone(), sink.clone());
            let thread = std::thread::Builder::new()
                .name(format!("capture-{}", source.as_str()))
                .spawn(move || capture_loop(source, o, k, s))
                .expect("capture thread");
            chans.push(Channel { stop, thread });
        }
    }

    pub fn pause(&self, sink: &dyn CaptureSink) {
        self.stop_threads();
        for s in [Source::User, Source::Remote] {
            sink.status(s, ComponentState::Off, t("пауза", "paused").into());
        }
    }

    pub fn resume(&self, sink: Arc<dyn CaptureSink>) {
        let o = self.opts.lock().unwrap().clone();
        if let Some(o) = o {
            self.start(o, sink);
        }
    }

    pub fn stop(&self) {
        self.stop_threads();
        *self.opts.lock().unwrap() = None;
    }

    fn stop_threads(&self) {
        let chans: Vec<Channel> = self.channels.lock().unwrap().drain(..).collect();
        for c in &chans {
            c.stop.store(true, Ordering::SeqCst);
        }
        for c in chans {
            let _ = c.thread.join();
        }
    }
}

fn capture_loop(source: Source, opts: CaptureOptions, sink: Arc<dyn CaptureSink>, stop: Arc<AtomicBool>) {
    let mut failures = 0u32;
    let mut reason: Option<&'static str> = None;
    while !stop.load(Ordering::SeqCst) {
        if let Some(why) = reason.take() {
            failures += 1;
            if failures > 5 {
                return;
            }
            sink.status(source, ComponentState::Warn, format!("{why} — {}", t("перепідключаюся…", "reconnecting…")));
            std::thread::sleep(Duration::from_millis(500));
        }
        let Some(device) = device_for(source, opts.mic_device_id.as_deref()) else {
            let msg = match source {
                Source::User => t("мікрофон недоступний: пристрій не знайдено. Виберіть інший у налаштуваннях", "microphone unavailable: device not found. Pick another in settings"),
                Source::Remote => t("системний звук недоступний: немає пристрою виводу", "system audio unavailable: no output device"),
            };
            sink.status(source, ComponentState::Error, msg.into());
            return;
        };
        let (tx, rx) = channel();
        let broken = Arc::new(AtomicBool::new(false));
        let (stream, rate) = match open_stream(&device, source, tx, broken.clone()) {
            Ok(s) => s,
            Err(e) => {
                let msg = match source {
                    Source::User => crate::tr!("мікрофон недоступний: {}. Виберіть інший пристрій у налаштуваннях", "microphone unavailable: {}. Pick another device in settings", e),
                    Source::Remote => crate::tr!("системний звук недоступний: {}", "system audio unavailable: {}", e),
                };
                sink.status(source, ComponentState::Error, msg);
                crate::db::diag("audio_error", serde_json::json!({ "source": source.as_str(), "error": e }));
                return;
            }
        };
        failures = 0; // only consecutive failures count toward the reopen cap
        let label = match source {
            Source::User => device.to_string(),
            Source::Remote => t("весь звук Windows (loopback)", "all Windows audio (loopback)").into(),
        };
        sink.status(source, ComponentState::Ok, label);
        let key = device_key(source, opts.mic_device_id.as_deref());
        reason = pump(source, &opts, sink.as_ref(), &stop, &rx, rate, &broken, key);
        drop(stream);
        sink.level(source, 0.0);
    }
}

/// Processes samples until stop or a device change; returns why it should reopen (None = stopped).
#[allow(clippy::too_many_arguments)]
fn pump(
    source: Source,
    opts: &CaptureOptions,
    sink: &dyn CaptureSink,
    stop: &AtomicBool,
    rx: &Receiver<Vec<f32>>,
    rate: u32,
    broken: &AtomicBool,
    key: Option<String>,
) -> Option<&'static str> {
    let mut rs = Resampler::new(rate);
    let mut vad = Vad::default();
    let mut buf: Vec<f32> = Vec::new();
    let mut level = 0f32;
    let mut last_level = Instant::now();
    let mut last_check = Instant::now();
    let mut out = Vec::new();
    let send = |u: Utterance, is_final: bool| {
        sink.audio(AudioChunk { session_id: opts.session_id.clone(), utterance_id: u.id, source, pcm: u.pcm, start_at: u.start_at, end_at: now_ms(), is_final });
    };
    let reason = loop {
        if stop.load(Ordering::SeqCst) {
            break None;
        }
        if broken.load(Ordering::SeqCst) {
            break Some(t("пристрій відключено", "device disconnected"));
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(samples) => {
                rs.process(&samples, &mut buf);
                let mut start = 0;
                while start + FRAME <= buf.len() {
                    let rms = vad.push(&buf[start..start + FRAME], now_ms(), &mut out);
                    level = (level * 0.85).max(rms);
                    start += FRAME;
                }
                buf.drain(..start);
                for o in out.drain(..) {
                    match o {
                        VadOut::Partial(u) => send(u, false),
                        VadOut::Final(u) => send(u, true),
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break Some(t("пристрій відключено", "device disconnected")),
        }
        if last_level.elapsed() >= Duration::from_millis(100) {
            sink.level(source, level);
            last_level = Instant::now();
        }
        // Loopback binds to the default output at open time; follow Windows when it switches (speakers → headphones).
        if last_check.elapsed() >= Duration::from_secs(1) {
            last_check = Instant::now();
            let now_key = device_key(source, opts.mic_device_id.as_deref());
            if now_key != key {
                break Some(match source {
                    Source::User => t("зміна мікрофона", "microphone changed"),
                    Source::Remote => t("зміна пристрою виводу", "output device changed"),
                });
            }
        }
    };
    vad.flush(&mut out);
    for o in out.drain(..) {
        if let VadOut::Final(u) = o {
            send(u, true);
        }
    }
    reason
}

/// «Чи чує мене / співрозмовника?»: a short level probe without ASR. Returns the peak RMS.
pub fn probe_level(source: Source, mic: Option<&str>, ms: u64, on_level: impl Fn(f32)) -> Result<f32, String> {
    let device = device_for(source, mic).ok_or_else(|| t("пристрій не знайдено", "device not found").to_string())?;
    let (tx, rx) = channel();
    let (_stream, _rate) = open_stream(&device, source, tx, Arc::new(AtomicBool::new(false)))?;
    let end = Instant::now() + Duration::from_millis(ms);
    let mut max = 0f32;
    while Instant::now() < end {
        let mut window: Vec<f32> = Vec::new();
        let tick = Instant::now() + Duration::from_millis(80);
        while let Ok(s) = rx.recv_timeout(tick.saturating_duration_since(Instant::now())) {
            window.extend(s);
            if Instant::now() >= tick {
                break;
            }
        }
        let rms = if window.is_empty() { 0.0 } else { (window.iter().map(|v| v * v).sum::<f32>() / window.len() as f32).sqrt() };
        max = max.max(rms);
        on_level(rms);
    }
    Ok(max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_keeps_low_tone_and_rate() {
        let mut r = Resampler::new(48_000);
        let input: Vec<f32> = (0..48_000).map(|n| (n as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.5).collect();
        let mut out = Vec::new();
        for c in input.chunks(480) {
            r.process(c, &mut out);
        }
        assert!((out.len() as i64 - 16_000).abs() < 40, "{}", out.len());
        let rms = (out[1000..].iter().map(|v| v * v).sum::<f32>() / (out.len() - 1000) as f32).sqrt();
        assert!((rms - 0.3535).abs() < 0.03, "{rms}");
    }

    #[test]
    fn vad_cuts_one_utterance() {
        let mut v = Vad::default();
        let mut out = Vec::new();
        let silence = vec![0.0f32; FRAME];
        let loud: Vec<f32> = (0..FRAME).map(|n| (n as f32 * 0.3).sin() * 0.2).collect();
        for i in 0..50 {
            v.push(&silence, i * 20, &mut out);
        }
        for i in 0..60 {
            v.push(&loud, 1000 + i * 20, &mut out);
        }
        for i in 0..40 {
            v.push(&silence, 2200 + i * 20, &mut out);
        }
        let finals: Vec<_> = out.iter().filter(|o| matches!(o, VadOut::Final(_))).collect();
        assert_eq!(finals.len(), 1);
        assert!(out.iter().any(|o| matches!(o, VadOut::Partial(_))));
    }
}
