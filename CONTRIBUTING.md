# Contributing

Bug reports and pull requests are welcome. For a bug, **Settings → Diagnostics → Save a report for support** gives a file with no keys and no conversation text.

## Checks

```powershell
cargo test
cargo clippy --all-targets -- -D warnings
```

Windows only, MSVC toolchain. To look at the UI without opening windows or touching your real data:

```powershell
$env:COPILOT_DATA_DIR = "$env:TEMP\copilot-test"
$env:COPILOT_SNAPSHOT = "$env:TEMP\copilot-shots"
$env:COPILOT_SNAPSHOT_LANGS = "en,de"
cargo run --release
```

## Add or fix a language

Every UI string is written in the code as `t("Ukrainian", "English")` or `tr!("Ukrainian", "English", args)`. Other languages live in `locales/<code>.json`, keyed by the English text.

```powershell
python scripts\i18n.py status            # how complete each catalog is
python scripts\i18n.py missing de        # what a catalog still needs
python scripts\i18n.py merge de new.json # merge your translations and drop stale entries
```

Keep placeholders (`{}`, `{:.1}`); use `{0}` and `{1}` if a language needs another order. A test fails when a catalog misses a string or a placeholder.

A new interface language also needs one `ui: true` entry in `LANGS` (`src/types.rs`) and one line in `catalog_source` (`src/i18n.rs`). A new speech language only needs the `LANGS` entry with `ui: false`. Question detection for a new language lives in `src/session/detector.rs`.

Arabic, Hebrew and Indic scripts work for speech and answers, but egui does not shape text, so they are not offered as interface languages.

## Style

Code reads like the code around it: small functions, few dependencies, comments that say why. Non-trivial logic gets a test. Anything touching a binary or model download pins its SHA-256, and `scripts/package.py` must match the app (`pins_match_the_app` checks that).

The Silero VAD weights in `resources/` come from the official ONNX export via `scripts/export_silero_weights.py`; `matches_onnx_runtime` pins the port's output.

By contributing you agree your work is released under the [MIT License](LICENSE).
