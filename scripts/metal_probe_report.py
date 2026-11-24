import json,statistics,sys
from pathlib import Path
src,out=map(Path,sys.argv[1:])
rows=[json.loads(l) for l in src.read_text().splitlines() if l.startswith("{")]
if not rows: raise RuntimeError("missing Metal capability record")
report={"schema":"herdr.metal-swizzle/v1","capability":rows[0],"comparisons":{}}
if rows[0]["status"]=="unavailable":
    report["status"]="unavailable"
elif rows[0]["status"]=="device_available":
    report["status"]="complete"
    for w,h in ((640,480),(1024,768),(1920,1080),(3840,2160)):
        pairs={}
        for row in rows[1:]:
            if (row["width"],row["height"])==(w,h):
                if row["status"]!="verified": raise RuntimeError("missing pixel proof")
                pair=pairs.setdefault(row["round"],{})
                if row["arm"] in pair: raise RuntimeError("duplicate observation")
                pair[row["arm"]]=row["ms"]
        if set(pairs)!=set(range(12)) or any(set(p)!={"cpu","metal"} or min(p.values())<=0 for p in pairs.values()):
            raise RuntimeError("incomplete pairs")
        deltas=[(p["metal"]/p["cpu"]-1)*100 for p in pairs.values()]
        report["comparisons"][f"{w}x{h}"]={"paired_median_pct":statistics.median(deltas),"paired_deltas_pct":deltas,
          "cpu_p50_ms":statistics.median(p["cpu"] for p in pairs.values()),
          "metal_p50_ms":statistics.median(p["metal"] for p in pairs.values())}
else: raise RuntimeError("invalid capability record")
out.write_text(json.dumps(report,indent=2)+"\n")
print(json.dumps(report,indent=2))
