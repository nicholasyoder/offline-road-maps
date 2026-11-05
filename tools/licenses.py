#!/usr/bin/env python3
"""Writes the third-party notices that travel with a release (docs/phase6-plan.md, 6.6).

    tools/licenses.py <out.txt>

The Rust crates that end up inside OfflineRoadMaps on Linux or Windows (`cargo tree -p
omp-app -e normal,no-proc-macro` for both targets; build tools and proc macros do not ship),
each with its declared licence and the licence files found in its package, plus the fonts
and icons bundled from outside crates. Texts that are word for word the same are printed
once, with the crates they belong to.

Fails (exit 1) when a crate's licence expression cannot be satisfied by the allowed list
below (so a GPL dependency cannot slip in unseen). A package with no licence file gets the
standard Apache-2.0 text if that is one of its licences; any other is listed in the output
and warned about on stderr.

Needs the crates downloaded (any build does that; `cargo fetch` for the other target).
Standard library only; run it from anywhere.
"""
import hashlib
import json
import os
import re
import subprocess
import sys

ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
TARGETS = ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"]

# Licences we accept to ship. An `OR` needs one of its sides here, an `AND` all of them.
ALLOWED = {
    "MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Zlib", "Unlicense",
    "BSL-1.0", "0BSD", "Unicode-3.0", "Unicode-DFS-2016", "OFL-1.1", "Ubuntu-font-1.0",
    "CC0-1.0", "MPL-2.0", "NCSA",
}

NOTICE_NAME = re.compile(r"(licen[cs]e|copying|unlicense|notice|copyright|\bofl\b|\bufl\b)", re.I)
NOTICE_DIRS = {"fonts", "license", "licenses", "licence", "licences", "legal"}


def run(*cmd):
    return subprocess.run(cmd, cwd=ROOT, check=True, capture_output=True, text=True).stdout


def satisfied(expr):
    """Whether the SPDX expression `expr` can be met with ALLOWED licences."""
    toks = re.findall(r"\(|\)|[^\s()/]+|/", expr.replace("/", " OR "))
    pos = 0

    def peek():
        return toks[pos] if pos < len(toks) else None

    def take():
        nonlocal pos
        pos += 1
        return toks[pos - 1]

    def atom():
        t = take()
        if t == "(":
            v = alt()
            take()  # ")"
            return v
        return t.rstrip("+") in ALLOWED or t in ALLOWED

    def conj():
        v = atom()
        while peek() == "AND":
            take()
            v = atom() and v
        return v

    def alt():
        v = conj()
        while peek() == "OR":
            take()
            v = conj() or v
        return v

    return alt()


def shipped():
    """{(name, version)} of the registry crates in the app, either target."""
    out = set()
    for t in TARGETS:
        text = run("cargo", "tree", "-p", "omp-app", "-e", "normal,no-proc-macro",
                   "--prefix", "none", "--target", t, "--locked")
        for line in text.splitlines():
            m = re.match(r"(\S+) v(\S+)(?: \((.*?)\))?", line)
            # A trailing "(/path)" marks a workspace crate; "(*)" only a repeat.
            if m and not (m.group(3) or "").startswith("/"):
                out.add((m.group(1), m.group(2)))
    return out


def notice_files(pkg_dir):
    found = []
    for name in sorted(os.listdir(pkg_dir)):
        p = os.path.join(pkg_dir, name)
        if os.path.isfile(p) and NOTICE_NAME.search(name) and not name.endswith((".rs", ".toml")):
            found.append(p)
        elif os.path.isdir(p) and name.lower() in NOTICE_DIRS:
            for sub in sorted(os.listdir(p)):
                q = os.path.join(p, sub)
                if os.path.isfile(q) and (sub.lower().endswith((".txt", ".md")) or NOTICE_NAME.search(sub)):
                    found.append(q)
    return found


def read(path):
    with open(path, encoding="utf-8", errors="replace") as f:
        return f.read().replace("\r\n", "\n").strip() + "\n"


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    meta = json.loads(run("cargo", "metadata", "--format-version", "1", "--locked"))
    pkgs = {(p["name"], p["version"]): p for p in meta["packages"]}
    wanted = sorted(shipped())

    bad, nofile = [], []
    texts = {}  # sha -> [text, [crate labels]]
    rows = []
    for key in wanted:
        p = pkgs.get(key)
        if p is None:
            sys.exit(f"licenses.py: {key[0]} {key[1]} is in the tree but not in cargo metadata")
        label = f"{p['name']} {p['version']}"
        lic = p.get("license") or ""
        if not lic or not satisfied(lic):
            bad.append(f"{label}: {lic or 'no licence declared'}")
        files = notice_files(os.path.dirname(p["manifest_path"]))
        if not files:
            if "Apache-2.0" in lic:
                # Published without its licence file (the egui and lyon crates): the licence
                # is the standard Apache-2.0 text, which this repository carries.
                t = read(os.path.join(ROOT, "LICENSE-APACHE"))
                h = hashlib.sha256(t.encode()).hexdigest()
                texts.setdefault(h, [t, []])[1].append(label + " (Apache-2.0 of its dual licence; the package has no licence file)")
            else:
                nofile.append(label)
        for f in files:
            t = read(f)
            h = hashlib.sha256(t.encode()).hexdigest()
            texts.setdefault(h, [t, []])[1].append(label)
        rows.append((label, lic, p.get("repository") or p.get("homepage") or ""))

    if bad:
        sys.exit("licenses.py: licences not on the allowed list:\n  " + "\n  ".join(bad))
    if nofile:
        print(f"licenses.py: warning: no licence file in {len(nofile)} packages: "
              + ", ".join(nofile), file=sys.stderr)

    version = re.search(r'^version\s*=\s*"(.*)"', open(os.path.join(ROOT, "Cargo.toml")).read()
                        .split("[workspace.package]")[1], re.M).group(1)
    bar = "=" * 80
    out = [f"OFFLINE ROAD MAPS {version}: THIRD-PARTY NOTICES", bar, "",
           "Offline Road Maps itself is licensed under the MIT or Apache-2.0 licence, at your",
           "choice (LICENSE-MIT, LICENSE-APACHE). It contains the following, under their own",
           "licences, which are printed below.", "", "",
           "1. FONTS, ICONS AND DATA", bar, ""]
    bundled = [
        ("Noto Sans (the map and interface font, subset)", "SIL Open Font License 1.1",
         os.path.join(ROOT, "crates/app/data/fonts/OFL.txt")),
        ("Maki map icons (Mapbox)", "CC0 1.0 (public domain dedication)",
         os.path.join(ROOT, "crates/app/data/icons/LICENSE.txt")),
    ]
    for what, lic, path in bundled:
        out += [f"* {what}: {lic}.", ""]
    out += ["* The fonts of egui's defaults (Ubuntu-Light, Hack, Noto Emoji, an emoji icon font)",
            "  come with the crate epaint_default_fonts below; their licences are among its",
            "  texts.",
            "* The time zone database inside jiff-tzdb is the IANA tz database (public domain).",
            "", "Licence texts of the fonts and icons above:", ""]
    for what, lic, path in bundled:
        out += [f"--- {what} ---", "", read(path), ""]

    out += ["", "2. RUST LIBRARIES", bar, "",
            f"{len(rows)} libraries are compiled into the program (the Linux and Windows builds",
            "together). Name, version, declared licence, home:", ""]
    for label, lic, repo in rows:
        out.append(f"* {label}: {lic}" + (f" ({repo})" if repo else ""))
    out += ["", "", "3. LICENCE TEXTS OF THE RUST LIBRARIES", bar, "",
            "Each text is printed once, followed by the libraries whose package contains it.", ""]
    for t, labels in sorted(texts.values(), key=lambda x: (-len(x[1]), x[1][0])):
        out += ["-" * 80, "Used by: " + ", ".join(labels), "-" * 80, "", t, ""]
    if nofile:
        out += ["-" * 80, "No licence file in the package of: " + ", ".join(nofile),
                "Their declared licences are given in section 2.", ""]

    with open(sys.argv[1], "w", encoding="utf-8", newline="\n") as f:
        f.write("\n".join(out))
    print(f"{sys.argv[1]}: {len(rows)} libraries, {len(texts)} distinct texts, "
          f"{len(nofile)} without a licence file")


if __name__ == "__main__":
    main()
