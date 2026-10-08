#!/usr/bin/env python3
"""Run not-hotdog with the compose pass over one whole intent (rev1), in an isolated session."""
import json, os, re, subprocess, sys, tempfile
HERE = os.path.dirname(os.path.abspath(__file__)); model, run = sys.argv[1], sys.argv[2]
SETTINGS = json.dumps({"enabledPlugins": {"legion@legion": False, "rafters@rafters": False, "playwright@claude-plugins-official": False, "security-guidance@claude-plugins-official": False}, "hooks": {}})
d = re.sub(r"Read the double diamond first:.*?\. ", "", open(os.path.join(HERE, "not-hotdog.md")).read())
r1 = json.load(open(os.path.join(HERE, "intent-rev1.json")))
st = [{"id": "what_it_is", "text": r1["what_it_is"]}, {"id": "becoming", "text": r1["direction"]["becoming"]}] + \
     [{"id": f"p{i+1}", "text": p["text"]} for i, p in enumerate(r1["direction"]["proposals"])]
prompt = d + "\n\n## Statements (one whole intent)\n\n" + "\n".join(json.dumps(s) for s in st) + "\n\nReturn only the JSON lines."
with tempfile.TemporaryDirectory() as cwd:
    r = subprocess.run(["claude", "-p", "--model", model, "--no-session-persistence", "--settings", SETTINGS,
                        "--disallowed-tools", "Bash,Read,Write,Edit,WebFetch,WebSearch,Agent"],
                       input=prompt, capture_output=True, text=True, cwd=cwd, timeout=900)
lines = [l.strip().strip("`") for l in r.stdout.splitlines() if l.strip().strip("`").startswith("{")]
comp = [json.loads(l) for l in lines if '"intent"' in l]
json.dump(comp[-1] if comp else {"raw": r.stdout[-2000:]}, open(os.path.join(HERE, f"compose-{model}-{run}.json"), "w"), indent=1)
print(model, run, "composed" if comp else "NO COMPOSE")
