#!/usr/bin/env python3
"""Same-runner, fixed-workload comparison. No network sites or synthetic results."""
import argparse
import collections
import hashlib
import http.server
import json
import math
import os
from pathlib import Path
import platform
import resource
import shutil
import signal
import statistics
import subprocess
import tempfile
import threading
import time
import traceback

ROOT = Path(__file__).resolve().parents[1]
BASELINE = "acf81de1738bf6ef2e4f6194208eb268e7523842"
PAIRS = 5
THRESHOLD = 15
FIXTURE = b"""<!doctype html><title>Herdr performance fixture</title>
<button id="action" style="width:140px;height:60px">Click</button>
<script>window.clicks=0;window.trusted=false;document.querySelector('#action').onclick=e=>{window.clicks++;window.trusted=e.isTrusted}</script>"""

class Fixture(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/html")
        self.send_header("Content-Length", str(len(FIXTURE)))
        self.end_headers()
        self.wfile.write(FIXTURE)
    def log_message(self, *_):
        pass

def run(*args, cwd=ROOT):
    return subprocess.check_output(args, cwd=cwd, text=True).strip()

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def build(tree, native, target):
    examples = tree / "examples"
    examples.mkdir(exist_ok=True)
    shutil.copyfile(ROOT / "benchmarks/workload.rs", examples / "perf_fixed.rs")
    names = ["perf_fixed"]
    if native:
        for case in ("wk_native_pixels", "wk_native_input"):
            name = "perf_" + case
            shutil.copyfile(ROOT / "benchmarks" / (case + ".rs"), examples / (name + ".rs"))
            names.append(name)
    args = ["cargo", "build", "--release", "--locked", "--target-dir", str(target)]
    for name in names:
        args += ["--example", name]
    subprocess.run(args, cwd=tree, check=True, timeout=2400)
    return {n: target / "release/examples" / n for n in names}

def usage():
    r = resource.getrusage(resource.RUSAGE_CHILDREN)
    return r.ru_utime + r.ru_stime

def rss_tree(pid):
    rows = {}
    for line in run("ps", "-axo", "pid=,ppid=,rss=").splitlines():
        p, parent, rss = map(int, line.split())
        rows[p] = (parent, rss)
    owned = {pid}
    while True:
        new = {p for p, (parent, _) in rows.items() if parent in owned}
        if new <= owned:
            break
        owned |= new
    return rows.get(pid, (0, 0))[1], sum(rows[p][1] for p in owned - {pid} if p in rows)

def measure(binary, suite, work, url, log):
    env = dict(os.environ, HERDR_APP_HOME=str(work / "app"), PERF_PROFILE=str(work / "profile"), PERF_FIXTURE_URL=url)
    env.pop("HERDR_WK_NATIVE_SMOKE", None)
    env.pop("HERDR_WK_NATIVE_INPUT_E2E", None)
    if suite == "wk_native_pixels":
        env["HERDR_WK_NATIVE_SMOKE"] = "1"
    elif suite == "wk_native_input":
        env["HERDR_WK_NATIVE_INPUT_E2E"] = "1"
    command = [str(binary)] + ([suite] if suite in ("portable", "chrome") else [])
    start = time.monotonic()
    cpu = usage()
    peaks = [0, 0]
    errors = []
    stop = threading.Event()
    with log.open("w") as output:
        proc = subprocess.Popen(command, env=env, stdout=output, stderr=subprocess.STDOUT, start_new_session=True)
        def sample_rss():
            while not stop.is_set():
                try:
                    current = rss_tree(proc.pid)
                    peaks[:] = [max(a, b) for a, b in zip(peaks, current)]
                except Exception as exc:
                    errors.append(str(exc))
                    return
                stop.wait(.2)
        monitor = None
        if suite != "portable":
            monitor = threading.Thread(target=sample_rss)
            monitor.start()
        try:
            code = proc.wait(timeout=240)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGKILL)
            proc.wait()
            raise
        finally:
            stop.set()
            if monitor:
                monitor.join()
    elapsed = (time.monotonic() - start) * 1000
    if code:
        raise RuntimeError(f"{suite} exited {code}; see {log.name}")
    result = collections.defaultdict(list)
    meta = {}
    for line in log.read_text(errors="replace").splitlines():
        if line.startswith("PERF "):
            row = json.loads(line[5:])
            value = row["ms"]
            if not math.isfinite(value) or value <= 0:
                raise ValueError(f"invalid observation: {row}")
            result[row["metric"] + "_ms"].append(value)
        elif line.startswith("PERF_META "):
            meta.update(json.loads(line[10:]))
    if suite == "portable" and any(len(result[k]) != 100 for k in ("url_policy_5000_calls_ms", "engine_policy_2000_calls_ms")):
        raise RuntimeError("incomplete portable samples")
    if suite == "chrome":
        expected = {"chrome_client_start":1,"chrome_session_open":3,"chrome_first_frame_receive":3,"chrome_query":60,"chrome_eval_roundtrip":60,"chrome_click_to_dom_observed":15,"chrome_screenshot_png":15,"chrome_session_close_confirmed":3,"chrome_client_shutdown":1}
        if any(len(result[k+"_ms"]) != n for k,n in expected.items()) or not meta.get("chrome_version"):
            raise RuntimeError("incomplete real Chrome workload")
    if suite.startswith("wk_"):
        marker = "native WKWebView screenshot pixel test passed" if suite.endswith("pixels") else "WK_TRUSTED_WHEEL_VERIFIED"
        if marker not in log.read_text(errors="replace"):
            raise RuntimeError("native test did not prove actual browser behavior")
    if suite != "portable":
        result[suite + "_workload_wall_ms"].append(elapsed)
        # Process-group CPU accounting includes sampler ps commands; label accordingly.
        result[suite + "_harness_and_waited_children_cpu_seconds"].append(usage() - cpu)
        if errors or not peaks[0]:
            raise RuntimeError("RSS sampling failed: " + str(errors))
        result[suite + "_harness_peak_rss_kib"].append(peaks[0])
        if suite == "chrome":
            if not peaks[1]:
                raise RuntimeError("Chrome descendant RSS not observed")
            result["chrome_descendants_peak_rss_kib"].append(peaks[1])
    return dict(result), meta

def percentile(values, p):
    return sorted(values)[max(0, math.ceil(p * len(values)) - 1)]

def summarize(data):
    return {"n":len(data),"p50":percentile(data,.50),"p95":percentile(data,.95),"p99":percentile(data,.99)}

def compare(rounds, suite):
    if len(rounds) != PAIRS:
        raise RuntimeError(f"{suite}: incomplete pairs")
    keys = set(rounds[0]["baseline"]["samples"])
    rows = []
    for pair in rounds:
        for side in ("baseline","candidate"):
            if set(pair[side]["samples"]) != keys:
                raise RuntimeError("metric coverage changed")
        if pair["baseline"]["meta"] != pair["candidate"]["meta"]:
            raise RuntimeError("browser version differs inside pair")
    for key in sorted(keys):
        base = [p["baseline"]["samples"][key] for p in rounds]
        head = [p["candidate"]["samples"][key] for p in rounds]
        a = summarize(sum(base, []))
        b = summarize(sum(head, []))
        changes = [(statistics.median(h)/statistics.median(v)-1)*100 for v,h in zip(base,head) if statistics.median(v)>0]
        delta = statistics.median(changes) if len(changes)==PAIRS else None
        # Screening only, not a statistical confidence interval. Require all five pairs.
        label = "inconclusive"
        if delta is not None and all(d >= THRESHOLD for d in changes):
            label = "regression signal"
        elif delta is not None and all(d <= -THRESHOLD for d in changes):
            label = "improvement signal"
        rows.append({"metric":key,"baseline":a,"candidate":b,"paired_delta_pct":delta,"pair_deltas_pct":changes,"classification":label})
    return rows

def report(data, out):
    (out/"performance.json").write_text(json.dumps(data,indent=2,allow_nan=False)+"\n")
    lines=["# Adapter performance comparison","",f"Status: **{data['status']}**", "",f"Baseline: `{data['baseline_sha']}`",f"Candidate: `{data['candidate_sha']}`","",f"Runner: {data['environment']}", "",
           "Lower is better. Delta = median of five paired run-median changes; ±15% in all five pairs is a screening signal, not statistical proof. Small sample tail percentiles are descriptive only.", "",
           "| Metric | Base n | Head n | Base p50 / p95 / p99 | Head p50 / p95 / p99 | Paired delta | Signal |","|---|---:|---:|---:|---:|---:|---|"]
    for suite, rows in data["comparison"].items():
        for r in rows:
            a,b=r["baseline"],r["candidate"]
            fmt=lambda x: " / ".join(f"{x[k]:.4f}" for k in ("p50","p95","p99"))
            delta="N/A" if r["paired_delta_pct"] is None else f"{r['paired_delta_pct']:+.1f}%"
            lines.append(f"| {r['metric']} | {a['n']} | {b['n']} | {fmt(a)} | {fmt(b)} | {delta} | {r['classification']} |")
    lines += ["","## Coverage and limitations", "", "- No full Herdr host dispatch, GPUI main-thread latency, presentation FPS, input-to-display latency, or full WebKit XPC process accounting in this standalone repo. Not measured; no claims.",
              "- Chrome first frame receive starts after session open; click latency ends at observed DOM, not display. WK metrics are full real smoke workload wall time including fixed waits and startup/teardown.",
              "- RSS is sampled peak, not exact high-water mark; descendants excludes reparented processes. CPU includes waited children and sampler overhead. Shared memory may be counted multiple times.",
              "- No architecture improvement is established by adding this harness. Correctness gates remain independent. Raw observations, lock hashes, workload hashes, environment and logs are in artifacts."]
    if data["errors"]:
        lines += ["","## Errors"]+["- "+str(e).replace("\n"," ") for e in data["errors"]]
    content="\n".join(lines)+"\n"
    (out/"performance.md").write_text(content)
    if os.getenv("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"],"a") as f:
            f.write(content)

def main():
    parser=argparse.ArgumentParser()
    parser.add_argument("--out",type=Path,required=True)
    parser.add_argument("--suite",choices=["portable","native"],required=True)
    args=parser.parse_args()
    out=args.out.resolve()
    out.mkdir(parents=True,exist_ok=True)
    native=args.suite=="native"
    head=run("git","rev-parse","HEAD")
    data={"schema":1,"baseline_sha":BASELINE,"candidate_sha":head,"status":"failed","comparison":{},"rounds":{},"errors":[],
          "environment":{"platform":platform.platform(),"machine":platform.machine(),"cpu_count":os.cpu_count(),"runner_image":os.getenv("ImageVersion"),"runner_os":os.getenv("RUNNER_OS"),"rust":run("rustc","-Vv"),"cargo":run("cargo","-V")},
          "workload_sha256":{p.name:digest(p) for p in (ROOT/"benchmarks").glob("*.rs")},"lock_sha256":{}}
    if native:
        data["environment"]["hardware"]=run("sysctl","-n","hw.model")
        data["environment"]["macos"]=run("sw_vers")
    else:
        data["environment"]["cpu_model"]=next((l.strip() for l in Path("/proc/cpuinfo").read_text().splitlines() if l.startswith("model name")), "unknown")
    with tempfile.TemporaryDirectory(prefix="herdr-perf-") as tmp:
        tmp=Path(tmp)
        trees={}
        server=None
        try:
            bins={}
            for side, sha in (("baseline",BASELINE),("candidate",head)):
                tree=tmp/side
                subprocess.run(["git","worktree","add","--detach",str(tree),sha],cwd=ROOT,check=True)
                trees[side]=tree
                # Baseline has no committed lock. Resolve it once, then reuse for
                # candidate unless its manifest/committed lock deliberately changes.
                lock=tree/"Cargo.lock"
                if not lock.exists() and side=="candidate":
                    shutil.copyfile(trees["baseline"]/"Cargo.lock",lock)
                if not lock.exists():
                    subprocess.run(["cargo","generate-lockfile"],cwd=tree,check=True,timeout=300)
                if side=="candidate" and not (trees["baseline"]/"Cargo.toml").read_bytes()==(tree/"Cargo.toml").read_bytes():
                    subprocess.run(["cargo","generate-lockfile"],cwd=tree,check=True,timeout=300)
                data["lock_sha256"][side]=digest(lock)
                shutil.copyfile(lock,out/(side+".Cargo.lock"))
                bins[side]=build(tree,native,tmp/(side+"-target"))
            server=http.server.ThreadingHTTPServer(("127.0.0.1",0),Fixture)
            thread=threading.Thread(target=server.serve_forever,daemon=True)
            thread.start()
            url=f"http://127.0.0.1:{server.server_port}/"
            suites=["chrome","wk_native_pixels","wk_native_input"] if native else ["portable"]
            for suite in suites:
                rounds=data["rounds"][suite]=[]
                for i in range(PAIRS):
                    pair={}
                    order=("baseline","candidate") if i%2==0 else ("candidate","baseline")
                    for side in order:
                        work=tmp/f"{suite}-{i}-{side}"
                        work.mkdir()
                        binary=bins[side]["perf_fixed" if suite in ("portable","chrome") else "perf_"+suite]
                        samples,meta=measure(binary,suite,work,url,out/f"{suite}-{i}-{side}.log")
                        pair[side]={"samples":samples,"meta":meta}
                    rounds.append(pair)
                data["comparison"][suite]=compare(rounds,suite)
            data["status"]="measured"
        except Exception as exc:
            data["errors"].append(str(exc))
            traceback.print_exc()
        finally:
            if server:
                server.shutdown()
                server.server_close()
            for tree in trees.values():
                subprocess.run(["git","worktree","remove","--force",str(tree)],cwd=ROOT,check=False)
            report(data,out)
    return 0 if data["status"]=="measured" else 1

if __name__=="__main__":
    raise SystemExit(main())
