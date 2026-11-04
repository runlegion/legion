#!/usr/bin/env python3
"""Second sample. Reads legion.db read-only. Row text goes only to the output dir.

  toy2.py sample <outdir>    draw 300 fresh rows, excluding the first sample
  toy2.py score <outdir>     compare labels-2a.json and labels-2b.json by group
"""
import collections
import json
import os
import random
import sqlite3
import sys

DB = os.path.expanduser("~/Library/Application Support/legion/legion.db")
SEED = 20261104
EXCERPT = 600
KIND_DOMAINS = {"gotcha", "incident", "rfc", "research", "feedback", "doctrine", "audit", "reference", "operating-case-law"}
TAILS = ("[SUBAGENT CHECKPOINT]", "[COMPACT CHECKPOINT]")


def rows():
    db = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
    names = ("id", "repo", "text", "created_at", "audience", "domain")
    return [dict(zip(names, r)) for r in db.execute(f"select {', '.join(names)} from reflections order by id")]


def group(row):
    text, domain = row["text"], row["domain"]
    if text.startswith(TAILS):
        return "tail"
    if text.startswith("[CHECKPOINT]"):
        return "checkpoint"
    if domain == "identity":
        return "identity"
    if domain in KIND_DOMAINS:
        return "kind-domain"
    if domain is None:
        return f"none-{row['audience']}"
    return None


WANT = {"identity": 60, "kind-domain": 60, "tail": 30, "checkpoint": 30, "none-self": 60, "none-team": 60}


def seen(out):
    ids = set()
    for name in ("a", "b"):
        with open(f"{out}/reader-{name}.jsonl") as f:
            ids |= {json.loads(line)["id"] for line in f}
    return ids


def sample(out):
    rng = random.Random(SEED)
    used = seen(out)
    pools = collections.defaultdict(list)
    for row in rows():
        name = group(row)
        if name and row["id"] not in used:
            pools[name].append(row)
    picked = []
    for name, want in WANT.items():
        pool = pools[name]
        rng.shuffle(pool)
        print(f"{name}: {min(want, len(pool))} of {len(pool)}")
        picked += pool[:want]
    rng.shuffle(picked)
    with open(f"{out}/reader-2.jsonl", "w") as f:
        for row in picked:
            f.write(json.dumps({
                "id": row["id"], "repo": row["repo"], "date": row["created_at"][:10],
                "audience": row["audience"], "chars": len(row["text"]), "text": row["text"][:EXCERPT],
            }) + "\n")
    print("sampled", len(picked))


def score(out):
    with open(f"{out}/labels-2a.json") as f:
        a = {r["id"]: r["kind"] for r in json.load(f)["rows"]}
    with open(f"{out}/labels-2b.json") as f:
        b = {r["id"]: r["kind"] for r in json.load(f)["rows"]}
    data = {r["id"]: r for r in rows()}
    ids = [i for i in a if i in b]
    print(f"rows {len(ids)}, agree {sum(a[i] == b[i] for i in ids)}")
    for name, labels in (("a", a), ("b", b)):
        print(name, collections.Counter(labels.values()).most_common())
    groups = collections.defaultdict(list)
    for i in ids:
        groups[group(data[i])].append(i)
    for name, members in groups.items():
        same = sum(a[i] == b[i] for i in members)
        print(f"{name}: {len(members)} rows, agree {same}")
        print("   a", collections.Counter(a[i] for i in members).most_common())
        print("   b", collections.Counter(b[i] for i in members).most_common())
    for kind in ("fact", "identity", "lesson", "finding", "decision", "state", "message", "musing", "noise", "does_not_fit"):
        either = [i for i in ids if kind in (a[i], b[i])]
        both = sum(a[i] == b[i] == kind for i in either)
        print(f"{kind}: either {len(either)}, both {both}")
    print("disagreements:", collections.Counter(tuple(sorted((a[i], b[i]))) for i in ids if a[i] != b[i]).most_common(14))


if __name__ == "__main__":
    {"sample": sample, "score": score}[sys.argv[1]](sys.argv[2])
