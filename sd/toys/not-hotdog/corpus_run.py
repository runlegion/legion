#!/usr/bin/env python3
"""Run not-hotdog (classify + compose) over every intent and thesis in corpus/, then summarize."""
import concurrent.futures as cf, glob, json, os, re, subprocess, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
SETTINGS = json.dumps({"enabledPlugins": {"legion@legion": False, "rafters@rafters": False,
                       "playwright@claude-plugins-official": False, "security-guidance@claude-plugins-official": False}, "hooks": {}})
DEF = re.sub(r"Read the double diamond first:.*?\. ", "", open(os.path.join(HERE, "not-hotdog.md")).read())
MECH = re.compile(r"\b(graph|node|edge|event log|stream|subscriber|uuid\w*|crate|daemon|cli|sqlite|sql|d1|r2|vector|cosine|"
                  r"schema|smugglr|eavesdrop|legion|rafters|veneer|kelex|zod|rust|tauri|server|sync\w*|api|mcp|socket|hook|"
                  r"ontology|cursor|precog|json|toml|worker\w*|cloudflare|github|forge|archivist|\d+(\.\d+)?%|\d+ ?ms)\b", re.I)

def run(path):
    doc = json.load(open(path))
    out = path.replace("/corpus/", "/corpus-out/")
    if os.path.exists(out): return json.load(open(out))
    prompt = (DEF + "\n\n## Statements (one whole intent)\n\n" + "\n".join(json.dumps(s) for s in doc["statements"])
              + "\n\nReturn only the JSON lines: one per statement, then the composed intent.")
    with tempfile.TemporaryDirectory() as cwd:
        r = subprocess.run(["claude", "-p", "--model", "opus", "--no-session-persistence", "--settings", SETTINGS,
                            "--disallowed-tools", "Bash,Read,Write,Edit,WebFetch,WebSearch,Agent"],
                           input=prompt, capture_output=True, text=True, cwd=cwd, timeout=1200)
    lines = []
    for l in r.stdout.splitlines():
        l = l.strip().strip("`")
        if l.startswith("{"):
            try: lines.append(json.loads(l))
            except json.JSONDecodeError: pass
    res = {**{k: doc[k] for k in ("id", "type", "status", "surface", "title")}, "n": len(doc["statements"]),
           "verdicts": [x for x in lines if "verdict" in x], "compose": next((x for x in lines if "intent" in x), None)}
    json.dump(res, open(out, "w"), indent=1)
    return res

os.makedirs(os.path.join(HERE, "corpus-out"), exist_ok=True)
paths = [p for p in sorted(glob.glob(os.path.join(HERE, "corpus", "*.json"))) if len(json.load(open(p))["statements"]) >= 5]
with cf.ThreadPoolExecutor(4) as ex: results = list(ex.map(run, paths))

rows, md = [], ["# not-hotdog over every intent and thesis\n"]
for r in results:
    v = [x["verdict"] for x in r["verdicts"]]
    c = r["compose"] or {}
    i = c.get("intent", {})
    comp = " ".join([i.get("what_it_is", ""), i.get("becoming", "")] + i.get("directions", []))
    own = {str(r["surface"]).lower().split("-")[0]}  # a service naming itself is not a mechanism
    leaks = sorted({m.group(0).lower() for m in MECH.finditer(comp)} - own)
    rows.append((r["type"], r["surface"], r["status"], r["n"], v.count("why"), v.count("not-why"), v.count("mixed"),
                 len(i.get("directions", [])), len(c.get("parked_for_workshop", [])), len(c.get("parked_for_spec", [])), leaks))
    md += [f"\n## {r['type']} {r['surface']} ({r['status']}) {r['id']}\n",
           f"**What it is:** {i.get('what_it_is','')}\n", f"**Becoming:** {i.get('becoming','')}\n", "**Directions:**"]
    md += [f"- {d}" for d in i.get("directions", [])]
    md += ["", f"Parked for the workshop ({len(c.get('parked_for_workshop', []))}):"] + [f"- {p}" for p in c.get("parked_for_workshop", [])]
    md += [f"\nParked for spec: {len(c.get('parked_for_spec', []))}. Mechanism words left in the composed intent: {', '.join(leaks) or 'none'}\n"]
open(os.path.join(HERE, "corpus-composed.md"), "w").write("\n".join(md))

print(f"{'type':7} {'surface':17} {'status':10} {'n':>3} {'why':>4} {'not':>4} {'mix':>4} {'dirs':>4} {'ws':>3} {'spec':>4}  leaks")
for t in rows: print(f"{t[0]:7} {str(t[1]):17} {str(t[2]):10} {t[3]:>3} {t[4]:>4} {t[5]:>4} {t[6]:>4} {t[7]:>4} {t[8]:>3} {t[9]:>4}  {', '.join(t[10])}")
tot = [sum(t[k] for t in rows) for k in (3, 4, 5, 6)]
print(f"\nall: {tot[0]} statements -> why {tot[1]}, not-why {tot[2]}, mixed {tot[3]}; "
      f"composed intents with no mechanism words: {sum(1 for t in rows if not t[10])}/{len(rows)}")
