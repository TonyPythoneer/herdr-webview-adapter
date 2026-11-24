#!/usr/bin/env python3
"""Candidate-only macOS stack sampling; isolated from all benchmark timings."""
import json
import math
import os
from pathlib import Path
import re
import signal
import statistics
import subprocess
import time

PROBE_METRICS = {
    "session_first_frame": ("chrome_session_open_ms", "chrome_first_frame_receive_ms"),
    "screenshot": ("chrome_screenshot_png_ms",),
    "eval_1mib": ("chrome_eval_1mib_ms",),
}
CATEGORIES = ("wait_or_sleep", "read_syscall", "write_syscall", "json", "base64",
              "allocation", "other")
SAMPLE_SECONDS = 2
INTERVAL_MS = 10
MAX_PROBES = 2
MAX_TEXT_BYTES = 8 * 1024 * 1024

def select_probes(rounds, comparison):
    """Prefer direct regression signals, then measured operation cost."""
    if not rounds:
        return []
    classifications = {row["metric"]: row["classification"] for row in comparison}
    ranked = []
    for probe, metrics in PROBE_METRICS.items():
        costs = [sum(sum(pair["candidate"]["samples"][key]) for key in metrics)
                 for pair in rounds]
        regression = any(classifications.get(key) == "regression signal" for key in metrics)
        ranked.append((not regression, -statistics.median(costs), probe))
    return [probe for _, _, probe in sorted(ranked)[:MAX_PROBES]]

def classify(symbol):
    symbol = symbol.lower()
    if any(s in symbol for s in ("psynch", "ulock_wait", "semaphore_wait", "mach_msg",
                                 "kevent", "nanosleep", "cond_wait", "thread_park", "poll")):
        return "wait_or_sleep"
    if "serde_json" in symbol or "simd_json" in symbol:
        return "json"
    syscall = symbol.strip().lstrip("_")
    if syscall in ("read", "read_nocancel", "readv", "pread"):
        return "read_syscall"
    if syscall in ("write", "write_nocancel", "writev", "pwrite"):
        return "write_syscall"
    if "base64" in symbol:
        return "base64"
    if any(s in symbol for s in ("malloc", "realloc", "free", "alloc::", "memcpy", "memmove")):
        return "allocation"
    return "other"

def summarize_sample(text):
    """Counts from Apple's collapsed top-of-stack table, not CPU percentages."""
    marker = "Sort by top of stack, same collapsed"
    if marker not in text:
        raise ValueError("sampler did not emit collapsed top-of-stack table")
    section = text.split(marker, 1)[1].splitlines()[1:]
    counts = dict.fromkeys(CATEGORIES, 0)
    for line in section:
        if line.strip().startswith("Binary Images:"):
            break
        match = re.fullmatch(r"\s*(.+?)\s+([0-9]+)\s*", line)
        if not match:
            continue
        symbol, n = match.groups()
        count = int(n)
        if count <= 0 or count > 10_000_000:
            raise ValueError("invalid stack sample count")
        counts[classify(symbol.split("  (in ", 1)[0])] += count
    if not 0 < sum(counts.values()) <= 10_000_000:
        raise ValueError("empty or oversized stack summary")
    return counts

def read_bounded(path):
    if path.stat().st_size > MAX_TEXT_BYTES:
        raise ValueError("diagnostic output exceeds bound")
    return path.read_text(errors="replace")

def run_probe(binary, operation, work, url, out):
    sampler = Path("/usr/bin/sample")
    result = {"operation": operation, "status": "unavailable"}
    if not sampler.is_file():
        result["reason"] = "sampler_unavailable"
        return result
    work.mkdir()
    log = out / f"profile-{operation}.log"
    sample = out / f"profile-{operation}.sample.txt"
    sampler_log = out / f"profile-{operation}.sampler.log"
    env = dict(os.environ, HERDR_APP_HOME=str(work / "app"),
               PERF_PROFILE=str(work / "profile"), PERF_FIXTURE_URL=url)
    proc = None
    try:
        with log.open("w") as output:
            proc = subprocess.Popen([str(binary), "profile_chrome", operation],
                                    env=env, stdout=output, stderr=subprocess.STDOUT,
                                    start_new_session=True)
            deadline = time.monotonic() + 40
            while "PROFILE_READY" not in read_bounded(log).splitlines():
                if proc.poll() is not None or time.monotonic() >= deadline:
                    result["reason"] = "probe_not_ready"
                    return result
                time.sleep(.05)
            with sampler_log.open("w") as diagnostic:
                sampled = subprocess.run([str(sampler), str(proc.pid), str(SAMPLE_SECONDS),
                                          str(INTERVAL_MS), "-file", str(sample)],
                                         stdout=diagnostic, stderr=subprocess.STDOUT,
                                         timeout=15, check=False)
            if sampled.returncode != 0:
                result["reason"] = "sampler_failed"
                return result
            if proc.wait(timeout=40) != 0:
                result["reason"] = "probe_failed"
                return result
        rows = [json.loads(line[len("PROFILE_RESULT "):])
                for line in read_bounded(log).splitlines() if line.startswith("PROFILE_RESULT ")]
        if len(rows) != 1:
            result["reason"] = "missing_operation_proof"
            return result
        proof = rows[0]
        n, elapsed = proof["operations"], proof["elapsed_ms"]
        if (type(n) is not int or not 1 <= n <= 4096 or type(elapsed) not in (int, float)
                or not math.isfinite(elapsed) or not 0 < elapsed <= 60_000):
            result["reason"] = "invalid_operation_proof"
            return result
        counts = summarize_sample(read_bounded(sample))
        return {"operation": operation, "status": "measured", "operations": n,
                "elapsed_ms": elapsed, "top_stack_counts": counts,
                "sample_seconds": SAMPLE_SECONDS, "interval_ms": INTERVAL_MS}
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
        result["reason"] = "diagnostic_failed"
        return result
    finally:
        if proc is not None:
            # Clean up the isolated process group even when the probe exits early.
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            proc.wait()

def diagnose(binary, rounds, comparison, work, url, out):
    selected = select_probes(rounds, comparison)
    return {"schema": 1, "method": "macos_sample_adapter_top_of_stack",
            "scope": "candidate_only_diagnostic", "probes": [
                run_probe(binary, probe, work / ("profile-" + probe), url, out)
                for probe in selected]}
