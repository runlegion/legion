"""Salience toy: three decay rules over legion's live reflections. Read-only."""
import math
import re
import sqlite3
import sys
from datetime import datetime, timezone

DB, NOW = sys.argv[1], datetime(2026, 10, 30, tzinfo=timezone.utc)
HALF, FLOOR = 30.0, 0.05
KEEP = {"identity", "workflow"}


def days(ts):
    ts = re.sub(r"(\.\d{6})\d+", r"\1", ts.replace("Z", "+00:00"))
    at = datetime.fromisoformat(ts)
    if at.tzinfo is None:
        at = at.replace(tzinfo=timezone.utc)
    return (NOW - at).total_seconds() / 86400


def age(r):
    return 0.5 ** (days(r["created_at"]) / HALF)


def use(r):
    since = days(r["last_recalled_at"] or r["created_at"])
    return min(1.0, (1 + 0.1 * r["recall_count"]) * 0.5 ** (since / HALF))


def keepers(r):
    return 1.0 if r["domain"] in KEEP or r["evergreen"] else use(r)


con = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
con.row_factory = sqlite3.Row
rows = [dict(r) for r in con.execute(
    "SELECT id, repo, domain, evergreen, created_at, recall_count, last_recalled_at "
    "FROM reflections WHERE deleted_at IS NULL")]
rules = {"A age": age, "B use": use, "C keepers": keepers}


def pct(n, d):
    return f"{100 * n / d:.1f}% ({n}/{d})" if d else "n/a"


rafters = [r for r in rows if r["repo"] == "rafters"]
march_used = [r for r in rafters if r["created_at"].startswith("2026-03") and r["recall_count"] >= 2]
used = [r for r in rafters if r["recall_count"] >= 2]
stale = [r for r in rows if r["recall_count"] == 0 and days(r["created_at"]) > 60]
kept = [r for r in rows if r["domain"] in KEEP]
print(f"memories {len(rows)}, rafters {len(rafters)}, rafters March recalled>=2: {len(march_used)}")
for name, f in rules.items():
    order = sorted(rafters, key=f, reverse=True)
    rank = {r["id"]: i for i, r in enumerate(order)}
    ranks = sorted(rank[r["id"]] / len(order) for r in march_used)
    median = ranks[len(ranks) // 2] if ranks else float("nan")
    print(f"\n== {name}")
    print(f"P1 March recalled>=2: median rank {100 * median:.1f}% of rafters "
          f"(top 10%: {pct(sum(x < 0.10 for x in ranks), len(ranks))}, "
          f"bottom half: {pct(sum(x >= 0.5 for x in ranks), len(ranks))})")
    print(f"P2 rafters recalled>=2 above floor: {pct(sum(f(r) >= FLOOR for r in used), len(used))}")
    print(f"P3 never recalled, older than 60 days, below floor: {pct(sum(f(r) < FLOOR for r in stale), len(stale))}")
    print(f"P4 identity/workflow below floor: {pct(sum(f(r) < FLOOR for r in kept), len(kept))}")
    print(f"P5 all memories above floor: {pct(sum(f(r) >= FLOOR for r in rows), len(rows))}")
