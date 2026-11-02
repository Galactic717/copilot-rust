# Changelog

## 1.0.0

### New
- **One file.** A single `.exe` holds everything: speech recognition, the voice detector and the local model server. It runs offline from the first launch, on a clean Windows with no VC++ redistributable.
- **Finds local LLMs on its own:** running servers (Ollama, LM Studio, Jan, GPT4All, llama.cpp, KoboldCpp and more), a stopped Ollama, and GGUF files from LM Studio, Hugging Face, Jan, GPT4All and Ollama. One click connects a model. If there is none, one click downloads a recommended one, with resume and checksum verification.
- **12 interface languages:** English, Ukrainian, German, Spanish, French, Italian, Portuguese, Polish, Turkish, Chinese, Japanese, Korean. The language follows Windows on first run.
- **47 languages for speech and answers.** Question detection understands 12 of them, including languages that do not separate words.
- **Mock interview:** questions from your CV and the job, answered aloud, then scored with a stronger version written out.
- **Debriefs and meeting notes** after a session, kept with it, searchable and exported.
- **Live translation** of questions into your language.
- **Self-update** with SHA-256 verification, start with Windows, a Start menu shortcut, and a diagnostics report for support.
- A local model can read screenshots when a vision projector sits beside it.

### Changed
- Voice detection is now plain Rust, compared frame by frame against ONNX Runtime with identical decisions. ONNX Runtime and DirectML are gone, and the app shrank from 51 MB to 30 MB.
- GPU layers for local models default to automatic.

### Fixed
- **A crash on the second Windows OCR call.** "Run check" or reading a screenshot with OCR could bring the whole app down.
- **Speech recognition failing for a Windows profile with a Cyrillic name** when the disk has 8.3 names disabled.
- The app would not start without the VC++ redistributable, or without `DirectML.dll` beside it.
- The "start automatically" switch for a local server was ignored.

## 0.3.0

Rewritten from Electron and TypeScript to Rust.
