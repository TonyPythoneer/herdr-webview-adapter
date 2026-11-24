#!/usr/bin/env python3
"""Select a coordinated GPUI dependency update before opening any PR."""
import argparse
import json
import re
import sys
import tomllib
import urllib.request
from pathlib import Path

VERSION = re.compile(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$")
INDEX_URL = "https://index.crates.io/gp/ui/gpui-wry"


def version_tuple(value):
    match = VERSION.fullmatch(value)
    if not match:
        raise ValueError(f"not a stable release: {value}")
    return tuple(map(int, match.groups()))


def exact_pin(value):
    if not isinstance(value, str) or not value.startswith("="):
        raise ValueError("expected exact dependency pin")
    version_tuple(value[1:])
    return value[1:]


def host_core(text):
    manifest = tomllib.loads(text)
    sections = [manifest, *manifest.get("target", {}).values()]
    pins = {"gpui-pre": set(), "gpui-pre-platform": set()}
    for section in sections:
        for kind in ("dependencies", "dev-dependencies", "build-dependencies"):
            for name, dep in section.get(kind, {}).items():
                if not isinstance(dep, dict):
                    continue
                package = dep.get("package", name)
                if package in pins:
                    pins[package].add(exact_pin(dep.get("version")))
    if len(pins["gpui-pre"]) != 1 or pins["gpui-pre"] != pins["gpui-pre-platform"]:
        raise ValueError("Host GPUI and platform pins must be exact and identical")
    return next(iter(pins["gpui-pre"]))


def adapter_wry_pin(text):
    manifest = tomllib.loads(text)
    return exact_pin(manifest["target"]["cfg(target_os = \"macos\")"]["dependencies"]["gpui-wry"])


def eligible_core(adapter, upstream, host):
    # An unchanged core never consults the dependent package registry.
    return (version_tuple(upstream) > version_tuple(adapter) and upstream == host)


def select_wry(records, current, target_core):
    current_version = version_tuple(current)
    candidates = []
    for record in records:
        try:
            candidate = version_tuple(record["vers"])
        except ValueError:
            continue
        # Patch series only; renames, minor/major API migrations are manual.
        if record.get("yanked") is not False or candidate[:2] != current_version[:2] or candidate <= current_version:
            continue
        gpui = [d for d in record["deps"]
                if (d.get("package") or d.get("name")) == "gpui-pre"
                and d.get("kind") in (None, "normal")]
        if gpui and all(d.get("req") == "=" + target_core for d in gpui):
            candidates.append((candidate, record["vers"]))
    return max(candidates)[1] if candidates else None


def rewrite_manifest(text, target_core, target_wry):
    def replace_pin(text, key, package, version):
        pattern = rf'(?m)^(\s*{key}\s*=\s*\{{\s*package\s*=\s*"{package}",\s*version\s*=\s*"=)[^"]+(")'
        result, n = re.subn(pattern, lambda m: m[1] + version + m[2], text)
        if n != 1:
            raise ValueError(f"unexpected Adapter {key} declaration count: {n}")
        return result
    text = replace_pin(text, "gpui", "gpui-pre", target_core)
    text = replace_pin(text, "gpui_platform", "gpui-pre-platform", target_core)
    text, n = re.subn(r'(?m)^(\s*gpui-wry\s*=\s*"=)[^"]+(")', lambda m: m[1] + target_wry + m[2], text)
    if n != 1 or host_core(text) != target_core or adapter_wry_pin(text) != target_wry:
        raise ValueError("rewritten manifest did not match selected pins")
    return text


def registry_records():
    request = urllib.request.Request(INDEX_URL, headers={"User-Agent": "herdr-webview-adapter-compat/1"})
    with urllib.request.urlopen(request, timeout=30) as response:
        return [json.loads(line) for line in response if line.strip()]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--adapter", type=Path, required=True)
    parser.add_argument("--upstream", type=Path, required=True)
    parser.add_argument("--host", type=Path, required=True)
    args = parser.parse_args()
    adapter = args.adapter.read_text()
    current, target = host_core(adapter), host_core(args.upstream.read_text())
    host = host_core(args.host.read_text())
    if not eligible_core(current, target, host):
        print(f"No coordinated upgrade: Adapter={current}, upstream={target}, Host={host}")
        return 3
    candidate = select_wry(registry_records(), adapter_wry_pin(adapter), target)
    if candidate is None:
        print(f"No non-yanked gpui-wry patch explicitly compatible with GPUI {target}")
        return 3
    args.adapter.write_text(rewrite_manifest(adapter, target, candidate))
    print(f"Selected GPUI {target}, gpui-wry {candidate}; Cargo resolution still required")
    return 0


if __name__ == "__main__":
    sys.exit(main())
