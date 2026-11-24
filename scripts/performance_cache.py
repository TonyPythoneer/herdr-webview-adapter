"""Content-addressed benchmark inputs and binaries; never caches observations."""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import tempfile

SCHEMA = 1
MAX_ENTRIES = 8
MAX_BYTES = 350 * 1024 * 1024
HEX = re.compile(r"[0-9a-f]{64}")
FIXTURE_NAMES = ("workload.rs", "wk_native_pixels.rs", "wk_native_input.rs", "fixture.html")

def digest(path):
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()

def content_key(inputs):
    return hashlib.sha256(json.dumps(inputs, sort_keys=True, separators=(",", ":"),
                                     allow_nan=False).encode()).hexdigest()

def fixture(cache, name, data):
    if name not in FIXTURE_NAMES:
        raise ValueError("unknown fixture")
    key = hashlib.sha256(data).hexdigest()
    path = cache / "fixtures" / key / name
    if path.is_file() and not path.is_symlink() and digest(path) == key:
        os.utime(path.parent, None)
        return path, True
    path.parent.mkdir(parents=True, exist_ok=True)
    # Replace, never follow a cached symlink.
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as tmp:
        tmp.write(data)
    os.replace(tmp.name, path)
    os.utime(path.parent, None)
    return path, False

def binary_names(native):
    return ("perf_fixed", "perf_wk_native_pixels", "perf_wk_native_input") if native else ("perf_fixed",)

def restore(cache, key, names, destination):
    entry = cache / "binaries" / key
    try:
        if not HEX.fullmatch(key) or entry.is_symlink():
            return None
        manifest = entry / "manifest.json"
        if not manifest.is_file() or manifest.is_symlink() or manifest.stat().st_size > 4096:
            return None
        meta = json.loads(manifest.read_text())
        if meta.get("schema") != SCHEMA or meta.get("key") != key or set(meta.get("sha256", {})) != set(names):
            return None
        hashes = meta["sha256"]
        for name in names:
            source = entry / name
            if (not source.is_file() or source.is_symlink() or not os.access(source, os.X_OK)
                    or not isinstance(hashes[name], str) or digest(source) != hashes[name]):
                return None
        destination.mkdir(parents=True, exist_ok=False)
        result = {}
        for name in names:
            output = destination / name
            shutil.copy2(entry / name, output)
            if digest(output) != hashes[name] or not os.access(output, os.X_OK):
                raise ValueError("restored binary failed verification")
            result[name] = output
        os.utime(entry, None)
        return result
    except (OSError, ValueError, TypeError, AttributeError):
        if destination.exists():
            shutil.rmtree(destination)
        return None

def save(cache, key, binaries):
    if not HEX.fullmatch(key):
        raise ValueError("invalid binary cache key")
    parent = cache / "binaries"
    parent.mkdir(parents=True, exist_ok=True)
    entry = parent / key
    with tempfile.TemporaryDirectory(dir=parent) as staging:
        source = Path(staging) / "entry"
        source.mkdir()
        hashes = {}
        for name, path in binaries.items():
            shutil.copy2(path, source / name)
            hashes[name] = digest(source / name)
        (source / "manifest.json").write_text(json.dumps({"schema": SCHEMA, "key": key, "sha256": hashes}))
        if entry.is_symlink():
            entry.unlink()
        elif entry.exists():
            shutil.rmtree(entry)
        source.rename(entry)

def prune(cache):
    parent = cache / "binaries"
    if not parent.is_dir():
        return
    entries = sorted((p for p in parent.iterdir() if p.is_dir() and not p.is_symlink() and HEX.fullmatch(p.name)),
                     key=lambda p: p.stat().st_mtime, reverse=True)
    total = 0
    for i, path in enumerate(entries):
        size = sum(p.stat().st_size for p in path.iterdir() if p.is_file() and not p.is_symlink())
        if i >= MAX_ENTRIES or total + size > MAX_BYTES:
            shutil.rmtree(path)
        else:
            total += size
    fixtures = cache / "fixtures"
    if fixtures.is_dir():
        entries = sorted((p for p in fixtures.iterdir() if p.is_dir() and not p.is_symlink() and HEX.fullmatch(p.name)),
                         key=lambda p: p.stat().st_mtime, reverse=True)
        for path in entries[16:]:
            shutil.rmtree(path)
