#!/usr/bin/env python3
"""Offline policy candidate: only previous observed gaps influence the next TTL."""
import argparse
import json
from pathlib import Path
import statistics

MIN_TTL, MAX_TTL = 1.0, 15.0
def adaptive(history):
    if not history:
        return 5.0
    # Upper median of recent observed gaps, clamped. No future information.
    sample = sorted(history[-8:])
    return min(MAX_TTL, max(MIN_TTL, sample[len(sample)//2] * 1.25))

def replay(gaps, choose):
    history, hits, held, misses = [], 0, 0.0, 0
    decisions = []
    for gap in gaps:
        ttl = choose(history)
        if not 0 <= ttl <= MAX_TTL or gap < 0:
            raise ValueError("invalid TTL or gap")
        hit = gap < ttl
        hits += hit
        misses += not hit
        held += min(ttl, gap)
        decisions.append({"gap_s":gap,"ttl_s":ttl,"hit":hit})
        history.append(gap)
    return {"hits":hits,"misses":misses,"idle_process_seconds":held,"decisions":decisions}

def main():
    p = argparse.ArgumentParser()
    p.add_argument("--out", type=Path, required=True)
    p.add_argument("--trace",type=Path, help="Optional JSON mapping anonymized trace names to idle gaps in seconds")
    args = p.parse_args()
    traces = json.loads(args.trace.read_text()) if args.trace else {
        "bursty":[0.5,1,2,0.5,2,1,30,0.5,1,2,1,2],
        "sparse":[30,60,120,25,60,30,90,60],
        "mixed":[1,2,8,12,1,2,25,9,1,30,2,12],
        "shift":[0.5,1,1,1,0.5,1,20,25,30,40,1,1]}
    result = {"schema":"herdr.idle-ttl-replay/v1","source":"provided" if args.trace else "synthetic",
              "note":"Idle process seconds is a proxy; not measured RSS or latency.",
              "traces":{}}
    for name,gaps in traces.items():
        result["traces"][name]={label:replay(gaps,policy) for label,policy in
            [("off",lambda _:0),("fixed_5",lambda _:5),("fixed_15",lambda _:15),("adaptive",adaptive)]}
    args.out.write_text(json.dumps(result,indent=2)+"\n")
    for name,policies in result["traces"].items():
        for policy,v in policies.items():
            print(name,policy,v["hits"],v["idle_process_seconds"])
if __name__ == "__main__": main()
