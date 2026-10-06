#!/usr/bin/env python3
"""Run not-hotdog over the gold statements with one model, in an isolated claude -p session."""
import json, os, random, re, subprocess, sys, tempfile

SETTINGS = json.dumps({"enabledPlugins": {"legion@legion": False, "rafters@rafters": False, "playwright@claude-plugins-official": False, "security-guidance@claude-plugins-official": False}, "hooks": {}})
HERE = os.path.dirname(os.path.abspath(__file__))
model, run = sys.argv[1], sys.argv[2]
definition = open(os.path.join(HERE, "not-hotdog.md")).read()
definition = re.sub(r"Read the double diamond first:.*?\. ", "", definition)  # no store access in the toy
gold = [json.loads(l) for l in open(os.path.join(HERE, "gold.jsonl"))]
random.seed(hash(run) & 0xFFFF)
random.shuffle(gold)

out = []
for i in range(0, len(gold), 12):
    batch = [{"id": g["id"], "text": g["text"]} for g in gold[i:i + 12]]
    prompt = (definition + "\n\n## Statements\n\n" + "\n".join(json.dumps(b) for b in batch)
              + "\n\nReturn only the JSON lines, one per statement, nothing else.")
    with tempfile.TemporaryDirectory() as cwd:
        r = subprocess.run(["claude", "-p", "--model", model, "--no-session-persistence", "--settings", SETTINGS,
                            "--disallowed-tools", "Bash,Read,Write,Edit,WebFetch,WebSearch,Agent"],
                           input=prompt, capture_output=True, text=True, cwd=cwd, timeout=600)
    for line in r.stdout.splitlines():
        line = line.strip().strip("`")
        if line.startswith("{"):
            try: out.append(json.loads(line))
            except json.JSONDecodeError: pass
    print(f"{model} batch {i // 12 + 1}: {len(out)} so far", file=sys.stderr)

with open(os.path.join(HERE, f"out-{model}-{run}.jsonl"), "w") as f:
    for o in out: f.write(json.dumps(o) + "\n")
