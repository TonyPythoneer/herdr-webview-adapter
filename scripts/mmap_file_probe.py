#!/usr/bin/env python3
"""Measure actual file-backed read vs mmap; no implication for CDP pipes."""
import argparse
import hashlib
import itertools
import json
import mmap
from pathlib import Path
import resource
import statistics
import tempfile
import time

def read_hash(path, method):
    before=resource.getrusage(resource.RUSAGE_SELF)
    start=time.perf_counter_ns()
    if method=="read":
        with path.open("rb") as f:
            digest=hashlib.sha256(f.read()).hexdigest()
    else:
        with path.open("rb") as f:
            with mmap.mmap(f.fileno(),0,access=mmap.ACCESS_READ) as view:
                digest=hashlib.sha256(view).hexdigest()
    elapsed=(time.perf_counter_ns()-start)/1e6
    after=resource.getrusage(resource.RUSAGE_SELF)
    return digest,{"ms":elapsed,"minor_faults":after.ru_minflt-before.ru_minflt,
                   "major_faults":after.ru_majflt-before.ru_majflt}

def main():
    p=argparse.ArgumentParser();p.add_argument("--out",type=Path,required=True);a=p.parse_args()
    report={"schema":"herdr.mmap-probe/v1","scope":"warm OS page cache; actual file hashing; not CDP transport", "sizes":{}}
    with tempfile.TemporaryDirectory(prefix="herdr-mmap-") as tmp:
        for size in (65536,1048576,4194304,16777216):
            path=Path(tmp)/"private-fixture.bin"
            data=(bytes(range(256))*(size//256))
            expected=hashlib.sha256(data).hexdigest()
            with path.open("wb") as f: f.write(data)
            del data
            for arm in ("read","mmap"):
                assert read_hash(path,arm)[0]==expected
            rounds=[]
            for i in range(12):
                order=("read","mmap") if i%2==0 else ("mmap","read")
                row={"order":order}
                for arm in order:
                    digest,obs=read_hash(path,arm)
                    assert digest==expected
                    row[arm]=obs
                rounds.append(row)
            delta=[(r["mmap"]["ms"]/r["read"]["ms"]-1)*100 for r in rounds]
            report["sizes"][str(size)]={"rounds":rounds,"paired_median_pct":statistics.median(delta),
              "read_p50_ms":statistics.median(r["read"]["ms"] for r in rounds),
              "mmap_p50_ms":statistics.median(r["mmap"]["ms"] for r in rounds)}
    a.out.write_text(json.dumps(report,indent=2)+"\n")
    for size,v in report["sizes"].items(): print(size,v["paired_median_pct"])
if __name__=="__main__": main()
