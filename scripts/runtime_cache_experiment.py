#!/usr/bin/env python3
"""Paired runtime-cache diagnostic; never reuses user profiles or observations."""
import argparse
import collections
import hashlib
import http.server
import itertools
import json
import math
import os
from pathlib import Path
import signal
import statistics
import subprocess
import tempfile
import threading

ARMS = ("cold", "disk", "retained")
HTML = b'<!doctype html><title>Cache experiment</title><script src="asset.js"></script><body>cache fixture</body>'
ASSET = b'window.cacheProof="cache-fixture-v1";\n/*' + b'x' * 131072 + b'*/'

class Fixture(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path.endswith("/index.html"):
            data, mime, cache = HTML, "text/html", "no-store"
        elif self.path.endswith("/asset.js"):
            data, mime, cache = ASSET, "application/javascript", "public, max-age=3600"
        else:
            self.send_error(404)
            return
        with self.server.lock:
            self.server.counts[self.path] += 1
        self.send_response(200)
        self.send_header("Content-Type", mime)
        self.send_header("Cache-Control", cache)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *_):
        pass

def invoke(binary, mode, profile, url, log):
    env = dict(os.environ, CACHE_PROFILE=str(profile), CACHE_URL=url,
               HERDR_APP_HOME=str(profile.parent / "isolated-app"))
    with log.open("w") as output:
        proc = subprocess.Popen([str(binary), mode], env=env, stdout=output,
                                stderr=subprocess.STDOUT, start_new_session=True)
        try:
            rc = proc.wait(timeout=120)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGKILL)
            proc.wait()
            raise
    raw = log.read_text(errors="replace")
    if rc:
        raise RuntimeError(f"{log.name}: exit {rc}: {raw[-4000:]}")
    rows = [json.loads(l.removeprefix("CACHE_RESULT ")) for l in raw.splitlines()
            if l.startswith("CACHE_RESULT ")]
    if len(rows) != 1 or rows[0]["mode"] != mode:
        raise RuntimeError(f"{log.name}: missing proof")
    row = rows[0]
    for key in ("start_ms", "open_ms", "frame_ms", "segments_ms"):
        if not math.isfinite(row[key]) or row[key] < 0 or (key != "start_ms" and row[key] == 0):
            raise ValueError(f"invalid {key}")
    return row

def disk_bytes(path):
    total = 0
    for root, _, files in os.walk(path, followlinks=False):
        for name in files:
            p = Path(root) / name
            try:
                if not p.is_symlink():
                    total += p.stat().st_size
            except FileNotFoundError:
                pass  # Chrome may remove a temporary file during shutdown.
    return total

def count(server, path):
    with server.lock:
        return server.counts[path + "/asset.js"], server.counts[path + "/index.html"]

def trial(binary, server, tmp, out, index, arm):
    path = f"/round-{index}/{arm}"
    url = f"http://127.0.0.1:{server.server_port}{path}/index.html"
    folder = tmp / f"{index}-{arm}"
    folder.mkdir()
    seed_profile = folder / "seed-profile"
    profile = folder / "fresh-profile" if arm == "cold" else seed_profile
    log = out / f"{index}-{arm}"
    if arm == "retained":
        row = invoke(binary, arm, profile, url, log.with_suffix(".log"))
        if count(server, path)[1] < 2 or count(server, path)[0] < 1:
            raise RuntimeError("retained arm did not prove two fresh documents")
        row["asset_requests_total"] = count(server, path)[0]
        row["seed_asset_requests"] = None
        row["reopen_asset_requests"] = None
    else:
        seed = invoke(binary, "seed", seed_profile, url, out / f"{index}-{arm}-seed.log")
        before, documents = count(server, path)
        if before < 1 or documents < 1:
            raise RuntimeError("seed did not fetch local fixture")
        row = invoke(binary, "restart", profile, url, log.with_suffix(".log"))
        after, new_documents = count(server, path)
        if seed["chrome_version"] != row["chrome_version"] or new_documents <= documents:
            raise RuntimeError("restart proof mismatch")
        if arm == "cold" and after <= before:
            raise RuntimeError("fresh profile did not fetch asset")
        row.update(seed_asset_requests=before, reopen_asset_requests=after-before,
                   asset_requests_total=after)
    row["profile_logical_bytes"] = disk_bytes(profile)
    return row

def comparison(rounds, candidate, reference):
    deltas = [(r[candidate]["segments_ms"] / r[reference]["segments_ms"] - 1) * 100
              for r in rounds]
    return {"paired_deltas_pct": deltas, "median_pct": statistics.median(deltas),
            "screening_signal": ("improvement" if all(d <= -15 for d in deltas)
                                 else "regression" if all(d >= 15 for d in deltas)
                                 else "inconclusive")}

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    report = {"schema": "herdr.runtime-cache-experiment/v1", "status": "incomplete",
              "sha": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
              "fixture_sha256": hashlib.sha256(HTML+ASSET).hexdigest(), "rounds": []}
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
    server.counts, server.lock = collections.Counter(), threading.Lock()
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix="herdr-cache-") as temp:
            for index, order in enumerate(itertools.permutations(ARMS)):
                row = {"order": list(order)}
                report["rounds"].append(row)
                for arm in order:
                    row[arm] = trial(args.binary.resolve(), server, Path(temp), args.out, index, arm)
        rounds = report["rounds"]
        versions = {r[a]["chrome_version"] for r in rounds for a in ARMS}
        if len(versions) != 1:
            raise RuntimeError("Chrome version changed")
        report["comparisons"] = {
            "disk_vs_cold": comparison(rounds, "disk", "cold"),
            "retained_vs_disk": comparison(rounds, "retained", "disk"),
            "retained_vs_cold": comparison(rounds, "retained", "cold"),
        }
        report["disk_asset_hit_rounds"] = sum(r["disk"]["reopen_asset_requests"] == 0 for r in rounds)
        report["retained_single_asset_request_rounds"] = sum(r["retained"]["asset_requests_total"] == 1 for r in rounds)
        report["median_segments_ms"] = {a: statistics.median(r[a]["segments_ms"] for r in rounds) for a in ARMS}
        report["median_idle_descendant_rss_kib"] = statistics.median(r["retained"]["idle_descendant_rss_kib"] for r in rounds)
        report["status"] = "complete"
        lines = ["# Runtime cache experiment", "", "| Arm | Median segments (ms) |",
                 "|---|---:|"] + [f"| {a} | {v:.2f} |" for a,v in report["median_segments_ms"].items()]
        lines += ["", "| Contrast | Paired median change | Screening |", "|---|---:|---|"]
        lines += [f"| {k} | {v['median_pct']:+.1f}% | {v['screening_signal']} |" for k,v in report["comparisons"].items()]
        lines += ["", f"Disk asset hit rounds: {report['disk_asset_hit_rounds']}/6.",
                  f"Retained single asset request rounds: {report['retained_single_asset_request_rounds']}/6.",
                  f"Retained descendant idle RSS: {report['median_idle_descendant_rss_kib']:.0f} KiB (summed RSS, not unique memory).",
                  "Segment sums exclude discovery, idle interval, DOM proof and native presentation.",
                  "Restart uses existing shutdown; disk cache survival is measured, not assumed.",
                  "Synthetic loopback workload; no release/merge approval or host paint claim."]
        summary = "\n".join(lines) + "\n"
        (args.out / "cache-reuse.md").write_text(summary)
        if os.getenv("GITHUB_STEP_SUMMARY"):
            with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as f:
                f.write(summary)
        print(summary)
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
        (args.out / "cache-reuse.json").write_text(json.dumps(report, indent=2) + "\n")

if __name__ == "__main__":
    main()
