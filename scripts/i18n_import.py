#!/usr/bin/env python3
"""Turns a numbered translation file into locales/<code>.json.

    python scripts/i18n.py numbered > en_keys.txt     (index<TAB>English, the stable order to translate against)
    python scripts/i18n_import.py de de.txt           (lines of `index|translation`)

Checks that every string is present and that placeholders match; leading spaces and a missing trailing space are kept
in step with the English text, because some strings are appended to others.
"""
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import i18n  # noqa: E402

SPEC = re.compile(r"\{[^{}]*\}")


def main(code, path):
    items = sorted(i18n.keys(), key=lambda s: (len(s), s))
    out, bad = {}, []
    for line in open(path, encoding="utf8"):
        line = line.rstrip("\n")
        if not line.strip():
            continue
        idx, text = line.split("|", 1)
        key = items[int(idx)]
        if key[0] == " " and not text.startswith(" "):
            text = " " + text
        if sorted(SPEC.findall(key)) != sorted(SPEC.findall(text)):
            bad.append(f"{idx}: {key!r} -> {text!r}")
        out[key] = text
    missing = [i for i, k in enumerate(items) if k not in out]
    if missing or bad:
        sys.exit(f"{code}: missing {missing[:20]}\n" + "\n".join(bad))
    i18n.save(code, out)
    print(code, "ok", len(out))


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
