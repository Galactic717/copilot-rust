#!/usr/bin/env python3
"""Translator's helper. Every UI string is written in the source as t("uk", "en") or tr!("uk", "en", args);
catalogs in locales/<code>.json map the English text to the translation.

    python scripts/i18n.py status          how complete each catalog is
    python scripts/i18n.py missing de      the English strings a catalog lacks, as a JSON object to fill in
    python scripts/i18n.py merge de f.json merge translations into locales/de.json and drop stale entries
    python scripts/i18n.py numbered        every string with a stable index (see i18n_import.py)

Placeholders ({} and {:.1}) must stay; a translation may reorder them with {0}, {1}.
"""
import glob
import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CALL = re.compile(r'(?:\bt|tr!)\(\s*"((?:[^"\\]|\\.)*)"\s*,\s*"((?:[^"\\]|\\.)*)"')


def unescape(s):
    return s.replace('\\"', '"').replace("\\n", "\n").replace("\\\\", "\\")


def keys():
    found = {}
    for path in sorted(glob.glob(os.path.join(ROOT, "src", "**", "*.rs"), recursive=True)):
        for m in CALL.finditer(open(path, encoding="utf8").read()):
            if m.group(2) != "en_text":
                found.setdefault(unescape(m.group(2)), unescape(m.group(1)))
    return found


def catalog_path(code):
    return os.path.join(ROOT, "locales", f"{code}.json")


def load(code):
    return json.load(open(catalog_path(code), encoding="utf8"))


def save(code, cat):
    with open(catalog_path(code), "w", encoding="utf8", newline="\n") as w:
        json.dump(dict(sorted(cat.items())), w, ensure_ascii=False, indent=0)
        w.write("\n")


def main():
    cmd = sys.argv[1] if len(sys.argv) > 1 else "status"
    src = keys()
    if cmd == "status":
        print(f"{len(src)} strings, {sum(len(k) for k in src)} characters")
        for path in sorted(glob.glob(os.path.join(ROOT, "locales", "*.json"))):
            code = os.path.basename(path)[:-5]
            cat = load(code)
            print(code, f"{sum(k in cat for k in src)}/{len(src)}", f"{sum(k not in src for k in cat)} stale")
    elif cmd == "missing":
        cat = load(sys.argv[2])
        print(json.dumps({k: "" for k in src if k not in cat}, ensure_ascii=False, indent=0))
    elif cmd == "numbered":
        for i, k in enumerate(sorted(src, key=lambda s: (len(s), s))):
            print(f"{i}	{k!r}")
    elif cmd == "merge":
        cat = load(sys.argv[2])
        cat.update(json.load(open(sys.argv[3], encoding="utf8")))
        save(sys.argv[2], {k: v for k, v in cat.items() if k in src and v})
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
