# Security

Please report a vulnerability privately through [GitHub Security Advisories](https://github.com/Galactic717/copilot-rust/security/advisories/new) rather than a public issue. Include the version, what an attacker can do, and steps to reproduce.

## What the app defends against

- **Prompt injection.** Documents, transcripts and screenshots are untrusted input: they are fenced in the prompt, and the model is told they are data, never instructions. Look-alike tags are normalized away first.
- **Key leaks.** API keys are encrypted with DPAPI, redacted from logs and exports, and refused inside URLs.
- **Tampered downloads.** Every downloaded binary and model is pinned to a SHA-256 digest, updates included. A mismatch deletes the file. Archive extraction rejects paths that escape the target folder.
- **Silent data flow.** All network traffic goes through one checked client. Offline mode is enforced there and re-checked on each redirect.

## Known limits

- The executable is not code-signed yet, so authenticity rests on HTTPS from GitHub plus the published checksums.
- DPAPI protects keys against other accounts and copied files, not against malware running as you.
- The capture-exclusion flag is a request to capture tools, which may ignore it. Do not rely on it to keep anything secret.
