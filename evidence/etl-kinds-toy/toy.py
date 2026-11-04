#!/usr/bin/env python3
"""ETL kinds toy. Reads legion.db read-only. Row text goes only to the output dir.

  toy.py sample <outdir>    draw the sample and write the two readers' files
  toy.py rules              run the mechanical rules over every row, print counts
  toy.py score <outdir>     score the readers' labels against each other and the rules
"""
import collections
import json
import os
import random
import re
import sqlite3
import sys

DB = os.path.expanduser("~/Library/Application Support/legion/legion.db")
SEED = 20261103
SIZE, SHARED, EACH = 500, 100, 200
EXCERPT = 600
VERBS = {"answer", "question", "request", "info", "routing", "status", "review"}
COLUMNS = "id, repo, text, created_at, audience, domain"


def rows():
    db = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
    return [
        dict(zip(("id", "repo", "text", "created_at", "audience", "domain"), r))
        for r in db.execute(f"select {COLUMNS} from reflections order by id")
    ]


def rule(row, repeats):
    text = row["text"]
    if row["domain"] == "checkpoint" or text.startswith("[CHECKPOINT]"):
        return "checkpoint"
    if repeats[text] >= 5 or len(text) < 20:
        return "noise"
    marker = re.match(r"\s*@\S+\s+(\w+)", text)
    if marker and marker.group(1) in VERBS:
        return marker.group(1)
    return None


def stratum(row):
    domain = row["domain"]
    state = "none" if domain is None else "checkpoint" if domain == "checkpoint" else "other"
    return row["audience"], state, row["created_at"][:7]


def draw(all_rows):
    """Takes an equal share from each stratum, then fills up at random."""
    rng = random.Random(SEED)
    strata = collections.defaultdict(list)
    for row in all_rows:
        strata[stratum(row)].append(row)
    share = max(1, SIZE // len(strata))
    picked = []
    for key in sorted(strata):
        members = strata[key]
        rng.shuffle(members)
        picked += members[:share]
    chosen = {row["id"] for row in picked}
    rest = [row for row in all_rows if row["id"] not in chosen]
    rng.shuffle(rest)
    picked += rest[: max(0, SIZE - len(picked))]
    rng.shuffle(picked)
    return picked[:SIZE]


def excerpt(row):
    return {
        "id": row["id"],
        "repo": row["repo"],
        "date": row["created_at"][:10],
        "audience": row["audience"],
        "chars": len(row["text"]),
        "text": row["text"][:EXCERPT],
    }


def sample(out):
    os.makedirs(out, exist_ok=True)
    picked = draw(rows())
    shared = picked[:SHARED]
    a = shared + picked[SHARED : SHARED + EACH]
    b = shared + picked[SHARED + EACH : SHARED + 2 * EACH]
    rng = random.Random(SEED + 1)
    for name, part in (("a", a), ("b", b)):
        rng.shuffle(part)
        with open(f"{out}/reader-{name}.jsonl", "w") as f:
            for row in part:
                f.write(json.dumps(excerpt(row)) + "\n")
    with open(f"{out}/shared-ids.json", "w") as f:
        json.dump([row["id"] for row in shared], f)
    print(f"sampled {len(picked)}, {len(a)} and {len(b)} per reader, {SHARED} shared")


def rules():
    all_rows = rows()
    repeats = collections.Counter(row["text"] for row in all_rows)
    counts = collections.Counter(rule(row, repeats) or "unsettled" for row in all_rows)
    total = len(all_rows)
    settled = total - counts["unsettled"]
    print(f"rows {total}, settled {settled} ({settled / total:.1%})")
    for name, n in counts.most_common():
        print(f"  {name}: {n}")


def labels(out, name):
    with open(f"{out}/labels-{name}.json") as f:
        return json.load(f)


def score(out):
    """Prints what a person or agent needs to match kinds by definition, then judge."""
    a, b = labels(out, "a"), labels(out, "b")
    with open(f"{out}/shared-ids.json") as f:
        shared = json.load(f)
    la = {row["id"]: row for row in a["rows"]}
    lb = {row["id"]: row for row in b["rows"]}
    print("kinds a", len(a["kinds"]), "kinds b", len(b["kinds"]))
    pairs = collections.Counter((la[i]["kind"], lb[i]["kind"]) for i in shared if i in la and i in lb)
    print("shared pairs:")
    for pair, n in pairs.most_common():
        print(f"  {n:3d} {pair[0]} / {pair[1]}")

    all_rows = {row["id"]: row for row in rows()}
    repeats = collections.Counter(row["text"] for row in all_rows.values())
    for name, doc in (("a", a), ("b", b)):
        n = len(doc["rows"])
        noise = sum(row["kind"] == "noise" for row in doc["rows"])
        multi = sum(bool(row.get("more_than_one")) for row in doc["rows"])
        print(f"reader {name}: {n} rows, noise {noise / n:.1%}, more than one kind {multi / n:.1%}")
        by_kind = collections.Counter(row["kind"] for row in doc["rows"])
        print("  ", by_kind.most_common())
        ruled = collections.Counter(
            (rule(all_rows[row["id"]], repeats), row["kind"])
            for row in doc["rows"]
            if rule(all_rows[row["id"]], repeats)
        )
        print("   rule / reader:", ruled.most_common())
        team = collections.Counter(row["kind"] for row in doc["rows"] if all_rows[row["id"]]["audience"] == "team")
        print("   team rows:", team.most_common())

    domains = collections.Counter(all_rows[row["id"]]["domain"] for doc in (a, b) for row in doc["rows"])
    print("domains in the sample:", domains.most_common())


if __name__ == "__main__":
    if sys.argv[1] == "sample":
        sample(sys.argv[2])
    elif sys.argv[1] == "rules":
        rules()
    elif sys.argv[1] == "score":
        score(sys.argv[2])
    else:
        sys.exit(__doc__)
