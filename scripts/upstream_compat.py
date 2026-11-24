#!/usr/bin/env python3
"""Read-only stable upstream compatibility preflight. Never executes upstream code."""
import argparse
import json
import re
import sys
from pathlib import Path

STABLE = re.compile(r"v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)")
REF = re.compile(r"([a-fA-F0-9]{40})\trefs/tags/(v[^\s]+)")
GPUI = re.compile(r'^gpui\s*=\s*\{\s*package\s*=\s*"gpui-pre",\s*version\s*=\s*"=([^"]+)"', re.M)
ANCHORS = {"src/window.rs": ["pub struct AppWindow"],
           "src/mods.rs": ["pub fn app_dir"],
           "src/commands.rs": ["pub const COMMANDS", "pub fn find(", "pub fn validate_args("],
           "Cargo.toml": ["gpui-pre"]}


def select_tag(text):
    candidates = []
    for line in text.splitlines():
        match = REF.fullmatch(line.strip())
        if match:
            sha, tag = match.groups()
            stable = STABLE.fullmatch(tag)
            if stable:
                candidates.append((tuple(map(int, stable.groups())), tag, sha.lower()))
    if not candidates:
        raise ValueError("no stable upstream tag found")
    _, tag, sha = max(candidates)
    return tag, sha


def gpui(text):
    m = GPUI.search(text)
    return m.group(1) if m else None


def probe(source, adapter, tag, sha, tag_object_sha):
    if (not STABLE.fullmatch(tag) or not re.fullmatch("[0-9a-f]{40}", sha)
            or not re.fullmatch("[0-9a-f]{40}", tag_object_sha)):
        raise ValueError("invalid pinned upstream commit/tag object identity")
    missing, upstream_cargo = [], ""
    for name, anchors in ANCHORS.items():
        path = source / name
        if not path.is_file():
            missing.append(name + ": missing file")
            continue
        text = path.read_text()
        if name == "Cargo.toml":
            upstream_cargo = text
        missing.extend(name + ": " + anchor for anchor in anchors if anchor not in text)
    adapter_gpui, upstream_gpui = gpui(adapter.read_text()), gpui(upstream_cargo)
    return {"schema": 2, "repository": "herdrdev/herdr-app", "tag": tag, "sha": sha,
            "tag_object_sha": tag_object_sha,
            "adapter_gpui": adapter_gpui, "upstream_gpui": upstream_gpui,
            "missing_anchors": missing,
            "compatible": not missing and adapter_gpui is not None and adapter_gpui == upstream_gpui,
            "scope": "Static anchors only; full Host cross-repo E2E remains mandatory."}


def gate(report, manifest):
    return (report.get("schema") == 2 and
            report.get("repository") == "herdrdev/herdr-app" and
            isinstance(report.get("tag"), str) and STABLE.fullmatch(report["tag"]) is not None and
            isinstance(report.get("sha"), str) and re.fullmatch("[0-9a-f]{40}", report["sha"]) is not None and
            isinstance(report.get("tag_object_sha"), str) and
            re.fullmatch("[0-9a-f]{40}", report["tag_object_sha"]) is not None and
            report.get("compatible") is True and not report.get("missing_anchors") and
            gpui(manifest) is not None and report.get("adapter_gpui") == gpui(manifest) and
            report.get("upstream_gpui") == gpui(manifest))


def main():
    p = argparse.ArgumentParser()
    sub = p.add_subparsers(dest="action", required=True)
    sub.add_parser("select")
    scan = sub.add_parser("probe")
    scan.add_argument("--source", type=Path, required=True)
    scan.add_argument("--tag", required=True)
    scan.add_argument("--sha", required=True)
    scan.add_argument("--tag-object-sha", required=True)
    verify = sub.add_parser("gate")
    verify.add_argument("record", type=Path)
    args = p.parse_args()
    if args.action == "select":
        print(*select_tag(sys.stdin.read()))
    elif args.action == "probe":
        print(json.dumps(probe(args.source, Path("Cargo.toml"), args.tag, args.sha,
                               args.tag_object_sha), indent=2))
    else:
        report = json.loads(args.record.read_text())
        if not gate(report, Path("Cargo.toml").read_text()):
            print("ERROR: upstream compatibility record is incomplete or incompatible", file=sys.stderr)
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
