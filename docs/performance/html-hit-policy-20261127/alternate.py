"""Run saved pre-change and current release benchmark executables serially."""
import json
import pathlib
import statistics
import subprocess

root = pathlib.Path(__file__).resolve().parents[3]
out = pathlib.Path(__file__).resolve().parent
bins = {"before": out / "before-bench-local", "after": root / "target/release/examples/font_lookup_latency"}
rows = []
for trial in range(3):
    for version in (["before", "after"] if trial % 2 == 0 else ["after", "before"]):
        result = subprocess.run([str(bins[version])], capture_output=True, text=True, check=True)
        (out / f"{version}-trial-{trial + 1}.csv").write_text(result.stdout)
        for line in result.stdout.splitlines():
            if line.startswith("html-cache,"):
                fields = line.split(",")
                rows.append(dict(trial=trial + 1, version=version, mode=fields[1], median_us=float(fields[2]), allocations=int(fields[5])))
summary = []
for mode in ("hit-off", "hit-on"):
    for version in ("before", "after"):
        sample = [r for r in rows if r["mode"] == mode and r["version"] == version]
        summary.append(dict(mode=mode, version=version, median_of_medians_us=statistics.median(r["median_us"] for r in sample), allocations=[r["allocations"] for r in sample]))
(out / "alternating.json").write_text(json.dumps(dict(rows=rows, summary=summary), indent=2) + "\n")
print(json.dumps(summary, indent=2))
