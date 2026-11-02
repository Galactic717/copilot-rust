# Privacy

Copilot has no server, no account and no telemetry. This page lists everything that leaves your computer and when.

## What stays on your computer

| Data | Where | Notes |
|---|---|---|
| Audio | Memory only | Recognized by whisper.cpp on this PC. Never written to disk, never sent anywhere. |
| Transcripts, questions, answers, debriefs | `%APPDATA%\copilot\kairos.db` (SQLite) | Only if you allow history. Delete a session or everything from History at any time. |
| CV, job description, notes | The same database | Text is extracted and indexed locally. |
| API keys | `%APPDATA%\copilot\keys.dpapi.json` | Encrypted by Windows DPAPI under your account, so a copy of the file is useless on another machine or account. |
| Diagnostics log | The same database | No keys and no conversation text. |

## What leaves your computer

| Request | Goes to | When |
|---|---|---|
| Question, recent transcript, relevant document fragments, profile notes | The answer model you selected | Each answer. With a local model it never leaves the PC. A cloud model needs your consent in Privacy settings first. |
| The selected screen area | The vision model you selected | Only after you select an area and press a button. |
| The whole transcript of a session | The model you selected | When a debrief is written: automatically only with a local model, on your click with a cloud one. |
| One GET to `api.github.com` | GitHub | On start, to check for a new version. It carries no data of yours. Turn it off in Settings → Hints; offline mode blocks it too. |
| Component and model downloads | GitHub and Hugging Face | Only for files you asked for. Every file is checked against a pinned SHA-256. |

A cloud provider handles what it receives under its own data policy. If that matters for a conversation, use a local model or turn on offline mode, which blocks every external address inside the app, redirects included.

## Local model discovery

Looking for local LLMs reads folders where such tools keep their models, and sends HTTP requests to `127.0.0.1` on their default ports. The results stay in memory and go nowhere.

## Telling others

Whether you may record a conversation depends on where you and the other participants are. Many places require everyone's consent. Telling participants, and checking the rules of the interview or meeting, is your responsibility.
