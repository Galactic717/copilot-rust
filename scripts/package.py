#!/usr/bin/env python3
"""Builds the release files from target/release/Copilot.exe.

    dist/Copilot-<version>-windows-x64.exe       one file with everything: runs offline right after download
    dist/Copilot-<version>-windows-x64-lite.exe  the app alone: fetches its components when first needed
    dist/SHA256SUMS.txt

The one-file edition is the exe with a zip appended. Windows runs the exe and ignores what follows it, and a zip is read
from its end, so the app can open itself as an archive (src/bundle.rs). Standard library only, so it runs anywhere CI does.
"""
import hashlib
import os
import re
import shutil
import sys
import urllib.request
import zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CACHE = os.path.join(ROOT, "target", "package-cache")
DIST = os.path.join(ROOT, "dist")

# The same pins as src/asr/assets.rs and src/discover.rs; `pins_match_the_app` in src/bundle.rs keeps them in step.
WHISPER = ("https://github.com/ggml-org/whisper.cpp/releases/download/b5454/whisper-bin-x64.zip", "6ba69e3482d7826214f90a6a9c84ca07782aec1e1d0c6a7c30c994fd5d816ccb")
BASE_MODEL = ("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin", "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe")
LLAMA = ("https://github.com/ggml-org/llama.cpp/releases/download/b12052/llama-b12052-bin-win-vulkan-x64.zip", "9806d110501576d9aa4a58de996209a89ffca3c0634e4bda54b0019dfafc3be5")


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def fetch(url, digest):
    os.makedirs(CACHE, exist_ok=True)
    path = os.path.join(CACHE, url.rsplit("/", 1)[1])
    if not (os.path.isfile(path) and sha256(path) == digest):
        print("downloading", url, flush=True)
        with urllib.request.urlopen(url) as r, open(path, "wb") as w:
            shutil.copyfileobj(r, w, 1 << 20)
        if sha256(path) != digest:
            os.remove(path)
            sys.exit(f"checksum mismatch: {url}")
    return path


def copy_entries(src_zip, out, prefix, keep):
    with zipfile.ZipFile(src_zip) as z:
        for info in z.infolist():
            name = info.filename.rsplit("/", 1)[-1]
            if not info.is_dir() and keep(name):
                out.writestr(prefix + info.filename, z.read(info), zipfile.ZIP_DEFLATED, 6)


def payload(path):
    whisper, model, llama = fetch(*WHISPER), fetch(*BASE_MODEL), fetch(*LLAMA)
    with zipfile.ZipFile(path, "w", allowZip64=True) as out:
        # Paths are relative to the data folder and match what AsrAssets and discover::Places look for.
        copy_entries(whisper, out, "asr/cpu/", lambda n: n.endswith(".dll") or n == "whisper-server.exe")
        out.write(model, "asr/models/ggml-base.bin", zipfile.ZIP_DEFLATED, 6)
        # Only the server: the llama-cli, llama-bench and similar tools in the archive are a third of its size.
        copy_entries(llama, out, "llama/", lambda n: n == "llama-server.exe" or (n.endswith(".dll") and "cli" not in n) or n.startswith("LICENSE"))


def main():
    version = re.search(r'^version = "(.+?)"', open(os.path.join(ROOT, "Cargo.toml"), encoding="utf8").read(), re.M).group(1)
    exe = os.path.join(ROOT, "target", "release", "Copilot.exe")
    if not os.path.isfile(exe):
        sys.exit("build first: cargo build --release")
    shutil.rmtree(DIST, ignore_errors=True)
    os.makedirs(DIST)
    base = f"Copilot-{version}-windows-x64"
    lite, full = os.path.join(DIST, f"{base}-lite.exe"), os.path.join(DIST, f"{base}.exe")
    shutil.copyfile(exe, lite)
    zip_path = os.path.join(CACHE, "payload.zip")
    os.makedirs(CACHE, exist_ok=True)
    payload(zip_path)
    with open(full, "wb") as w:
        for part in (exe, zip_path):
            with open(part, "rb") as r:
                shutil.copyfileobj(r, w, 1 << 20)
    os.remove(zip_path)
    with open(os.path.join(DIST, "SHA256SUMS.txt"), "w", newline="\n") as w:
        for p in (full, lite):
            w.write(f"{sha256(p)}  {os.path.basename(p)}\n")
            print(os.path.basename(p), os.path.getsize(p) // 1048576, "MB")


if __name__ == "__main__":
    main()
