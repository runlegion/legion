#!/usr/bin/env python3
"""Score not-hotdog runs: verdict accuracy, run-to-run agreement, and mechanism leaking into what it keeps."""
import collections, glob, json, os, re

HERE = os.path.dirname(os.path.abspath(__file__))
gold = {g["id"]: g for g in (json.loads(l) for l in open(os.path.join(HERE, "gold.jsonl")))}
MECH = re.compile(r"\b(graph|node|edge|event log|log|stream|subscriber|uuid\w*|label|crate|daemon|cli|sqlite|sql|vector|cosine|"
                  r"fts\w*|schema|smugglr|eavesdrop|server|sync\w*|api|socket|mod|hook|ontology|relation\w*|cursor|precog|"
                  r"uncertainty engine|witness\w*|toy|ms|\d+(\.\d+)?%|\d+ of \d+)\b", re.I)
runs = {}
for f in sorted(glob.glob(os.path.join(HERE, "out-*.jsonl"))):
    runs[os.path.basename(f)[4:-6]] = {o["id"]: o for o in (json.loads(l) for l in open(f))}

for name, out in runs.items():
    cm = collections.Counter((gold[i]["gold"], out[i]["verdict"]) for i in gold if i in out)
    right = sum(v for (g, p), v in cm.items() if g == p)
    leaks = [(i, m.group(0)) for i in out for m in [MECH.search(out[i].get("keep") or "")] if m]
    print(f"\n== {name}: {right}/{len(gold)} verdicts match gold; missing {len(set(gold) - set(out))}")
    for g in ("why", "not-why", "mixed"):
        print(f"   gold {g:8}: " + ", ".join(f"{p}={cm[(g, p)]}" for p in ("why", "not-why", "mixed") if cm[(g, p)]))
    print(f"   mechanism words left in 'keep': {len(leaks)} {leaks[:8]}")

names = list(runs)
for a in range(len(names)):
    for b in range(a + 1, len(names)):
        x, y = runs[names[a]], runs[names[b]]
        same = sum(x[i]["verdict"] == y[i]["verdict"] for i in gold if i in x and i in y)
        print(f"agreement {names[a]} vs {names[b]}: {same}/{len(gold)}")

print("\n== items where any run differs from gold")
for i, g in gold.items():
    vs = {n: r.get(i, {}).get("verdict") for n, r in runs.items()}
    if any(v != g["gold"] for v in vs.values()):
        print(f"- {i} gold={g['gold']} {vs} | {g['text'][:110]}")
        for n, r in runs.items():
            o = r.get(i, {})
            print(f"    {n}: keep='{(o.get('keep') or '')[:140]}' park='{(o.get('park') or '')[:80]}'")
