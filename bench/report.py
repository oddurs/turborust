#!/usr/bin/env python3
"""Turns bench/compare.py's JSON lines into the tables in docs/benchmarks.md.

    python3 bench/report.py results.jsonl
"""
import json, sys

rows = {}  # scenario -> tool -> result, in arrival order
order = []
scenario_of = {
    "lifecycle": lambda r: isinstance(r, dict) and "1" in r,
    "stubborn": lambda r: set(r) == {"stop_s", "leaked"},
    "killed": lambda r: set(r) == {"leaked"},
    "throughput": lambda r: "lines_per_s" in r,
    "longline": lambda r: "completed" in r,
    "flood": lambda r: "growth_mb" in r,
    "churn": lambda r: "kills" in r,
    "watch": lambda r: "median_ms" in r or r.get("failed"),
    "coalesce": lambda r: "atomic_save" in r,
}
for line in open(sys.argv[1]):
    line = line.strip()
    if not line:
        continue
    (tool, res), = json.loads(line).items()
    kind = next(k for k, test in scenario_of.items() if test(res))
    if kind not in rows:
        rows[kind] = {}
        order.append(kind)
    rows[kind][tool] = res


def ms(x):
    return "—" if x is None else f"{x * 1000:.0f} ms" if x >= 0.0095 else f"{x * 1000:.1f} ms"


def table(head, body):
    out = ["| " + " | ".join(head) + " |", "|" + "---|" * len(head)]
    out += ["| " + " | ".join(map(str, r)) + " |" for r in body]
    return "\n".join(out)


for kind in order:
    r = rows[kind]
    tools = list(r)
    if kind == "lifecycle":
        for n in ("1", "10", "50"):
            print(f"\n#### {n} service{'s' if n != '1' else ''}\n")
            body = []
            for t in tools:
                x = r[t][n]
                body.append([t, ms(x["start_s"]), ms(x["stop_s"]), f"{x['idle_cpu_pct']:.1f}%",
                             f"{x['pss_mb']:.1f} MB", x["leaked"]])
            print(table(["tool", "start", "stop", "idle CPU", "memory (PSS)", "left running"], body))
    elif kind == "stubborn":
        print("\n#### A child that ignores SIGTERM, with a grandchild\n")
        print(table(["tool", "stopped in", "left running"],
                    [[t, ms(r[t]["stop_s"]) if r[t]["stop_s"] else "never", r[t]["leaked"]] for t in tools]))
    elif kind == "killed":
        print("\n#### The supervisor is SIGKILLed\n")
        print(table(["tool", "processes left running"], [[t, r[t]["leaked"]] for t in tools]))
    elif kind == "throughput":
        print("\n#### One service printing 1,000,000 lines\n")
        print(table(["tool", "time", "lines/s", "supervisor CPU"],
                    [[t, ms(r[t]["seconds"]), f"{r[t]['lines_per_s'] / 1e6:.2f} M",
                      f"{r[t]['supervisor_cpu_s']:.2f} s"] for t in tools]))
    elif kind == "longline":
        print("\n#### One 64 MiB line with no newline\n")
        print(table(["tool", "memory afterwards"], [[t, f"{r[t]['pss_mb']:.0f} MB"] for t in tools]))
    elif kind == "flood":
        print("\n#### An error line printed nonstop for 20 s\n")
        print(table(["tool", "memory at 2 s", "memory at 20 s", "growth"],
                    [[t, f"{r[t]['pss_mb_every_2s'][0]} MB", f"{r[t]['pss_mb_every_2s'][-1]} MB",
                      f"{r[t]['growth_mb']} MB"] for t in tools]))
    elif kind == "churn":
        print("\n#### 20 services, a random one SIGKILLed every 50 ms for 20 s\n")
        print(table(["tool", "kills", "all back", "back within", "left running after stop"],
                    [[t, r[t]["kills"], f"{r[t]['alive_after']}/{r[t]['of']}", f"{r[t]['recovered_s']:.1f} s",
                      r[t]["leaked"]] for t in tools]))
    elif kind == "watch":
        print("\n#### Edit to restarted process, default settings\n")
        print(table(["tool", "median", "p90", "missed"],
                    [[t, f"{r[t]['median_ms']:.0f} ms", f"{r[t]['p90_ms']:.0f} ms", r[t]["missed"]]
                     if "median_ms" in r[t] else [t, "failed", "", ""] for t in tools]))
    elif kind == "coalesce":
        print("\n#### Restarts caused by one logical change (three runs each)\n")
        print(table(["tool", "atomic save", "200 files at once", "20 files, 5 ms apart"],
                    [[t, *("/".join(map(str, r[t][k])) for k in ("atomic_save", "checkout_200", "formatter_20x5ms"))]
                     for t in tools]))
