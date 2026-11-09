#!/usr/bin/env python3
"""Collects the texts to translate and updates the translations.

The .slint files mark them with @tr("..."), the Rust code with tr("...") and
trf("...", ...). This writes translations/slingshot.pot, and adds the new
texts (untranslated) to each translations/<lang>/LC_MESSAGES/slingshot.po,
keeping the existing translations; texts no longer used are dropped.
"""
import glob, os, re

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
STRING = r'"((?:[^"\\]|\\.)*)"'


def unescape(s):
    return re.sub(r'\\(.)', lambda m: {'n': '\n', 't': '\t'}.get(m.group(1), m.group(1)), s)


def escape(s):
    return s.replace('\\', '\\\\').replace('"', '\\"').replace('\n', '\\n').replace('\t', '\\t')


def collect():
    texts = {}
    for path in sorted(glob.glob(os.path.join(ROOT, "ui", "*.slint"))):
        for n, line in enumerate(open(path, encoding="utf-8"), 1):
            for m in re.finditer(r'@tr\(\s*' + STRING, line):
                texts.setdefault(unescape(m.group(1)), []).append(f"{os.path.relpath(path, ROOT)}:{n}")
    for path in sorted(glob.glob(os.path.join(ROOT, "src", "**", "*.rs"), recursive=True)):
        if path.endswith(os.path.join("src", "tr.rs")):
            continue
        for n, line in enumerate(open(path, encoding="utf-8"), 1):
            for m in re.finditer(r'\btrf?\(\s*' + STRING, line):
                texts.setdefault(unescape(m.group(1)), []).append(f"{os.path.relpath(path, ROOT)}:{n}")
    return texts


def read_po(path):
    """{msgid: msgstr}"""
    entries, id_, str_, field = {}, None, "", 0
    def flush():
        if id_ is not None and id_ != "":
            entries[id_] = str_
    for line in open(path, encoding="utf-8"):
        line = line.strip()
        if line.startswith("msgid "):
            if field == 2:
                flush()
            id_, str_, field = unescape(line[7:-1]), "", 1
        elif line.startswith("msgstr "):
            str_, field = unescape(line[8:-1]), 2
        elif line.startswith('"'):
            if field == 1:
                id_ += unescape(line[1:-1])
            elif field == 2:
                str_ += unescape(line[1:-1])
    if field == 2:
        flush()
    return entries


def write_po(path, texts, translations, header):
    with open(path, "w", encoding="utf-8") as f:
        f.write(header)
        for text in sorted(texts):
            f.write("\n")
            for where in texts[text][:3]:
                f.write(f"#: {where}\n")
            f.write(f'msgid "{escape(text)}"\n')
            f.write(f'msgstr "{escape(translations.get(text, ""))}"\n')


HEADER = '''msgid ""
msgstr ""
"Content-Type: text/plain; charset=UTF-8\\n"
"Language: {lang}\\n"
'''

if __name__ == "__main__":
    texts = collect()
    write_po(os.path.join(ROOT, "translations", "slingshot.pot"), texts, {}, HEADER.format(lang=""))
    for po in sorted(glob.glob(os.path.join(ROOT, "translations", "*", "LC_MESSAGES", "slingshot.po"))):
        lang = po.split(os.sep)[-3]
        old = read_po(po)
        write_po(po, texts, old, HEADER.format(lang=lang))
        missing = [t for t in texts if not old.get(t)]
        print(f"{lang}: {len(texts) - len(missing)} of {len(texts)} translated")
    print(f"{len(texts)} texts")
    # The firmware draws the translations' letters from Inter, or the Chinese
    # font's subset.
    letters = set()
    for po in glob.glob(os.path.join(ROOT, "translations", "*", "LC_MESSAGES", "slingshot.po")):
        for text in read_po(po).values():
            letters.update(c for c in text if ord(c) > 0x7e)
    try:
        from fontTools.ttLib import TTFont
    except ImportError:
        print("(fontTools not installed: the fonts' coverage isn't checked)")
    else:
        covered = set()
        for font in ("InterVariable.ttf", "NotoSansSC-subset.ttf"):
            covered.update(chr(c) for c in TTFont(os.path.join(ROOT, "ui", "fonts", font)).getBestCmap())
        missing = "".join(sorted(letters - covered))
        if missing:
            print(f"No font has {missing}: add them to ui/fonts/NotoSansSC-subset.ttf (AGENTS.md)")
