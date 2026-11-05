#!/usr/bin/env python3
"""Precog toy. Reads session transcripts and legion.db read-only; runs `legion recall`.

  toy.py run <out.jsonl> <transcript.jsonl>...   one row per tool call
  toy.py score <out.jsonl>                        the numbers the predictions name
"""
import json
import os
import re
import sqlite3
import statistics
import subprocess
import sys
import time

DB = os.path.expanduser("~/Library/Application Support/legion/legion.db")
SYM = "01a1a98c-6801-7480-ae1f-b988000b4a61"
SEARCH = re.compile(r"\b(grep|rg|find|ag)\b|Grep|Glob")
ID = re.compile(r"\b01[0-9a-f]{6}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b")


def text_of(content):
    if isinstance(content, str):
        return content
    out = []
    for block in content or []:
        if not isinstance(block, dict):
            continue
        kind = block.get("type")
        if kind == "text":
            out.append(block.get("text", ""))
        elif kind == "tool_use":
            out.append(json.dumps(block.get("input", {})))
        elif kind == "tool_result":
            out.append(text_of(block.get("content")))
    return "\n".join(out)


def moments(path):
    """Yields (timestamp, user text, action text, context so far, turn index) per tool call."""
    context, turns, last_user = [], 0, ""
    for line in open(path):
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get("type") not in ("user", "assistant"):
            continue
        content = row.get("message", {}).get("content")
        if row["type"] == "user":
            plain = content if isinstance(content, str) else "\n".join(
                b.get("text", "") for b in content or [] if isinstance(b, dict) and b.get("type") == "text")
            if plain.strip():
                last_user = plain[-600:]
        for block in content if isinstance(content, list) else []:
            if isinstance(block, dict) and block.get("type") == "tool_use":
                args = block.get("input", {})
                action = f"{block.get('name')} {args.get('description', '')} {args.get('command', args.get('pattern', ''))}"
                yield row.get("timestamp", ""), last_user, action[:400], "\n".join(context), turns
        context.append(text_of(content))
        turns += 1


def recall(text):
    began = time.perf_counter()
    out = subprocess.run(["legion", "recall", "--repo", "legion", "--context", text[:1500], "--limit", "3"],
                         capture_output=True, text=True, timeout=60).stdout
    return ID.findall(out)[:3], (time.perf_counter() - began) * 1000


def openers(ids):
    db = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
    found = {}
    for rid in ids:
        row = db.execute("select text from reflections where id = ?", (rid,)).fetchone()
        if row:
            line = next((part.strip() for part in row[0].split("\n") if len(part.strip()) > 30), row[0])
            found[rid] = line[:60]
    return found


def in_context(rid, opener, context):
    """Turns back where the reflection shows, by id or by its opening 60 characters, else None."""
    for needle in (rid, opener):
        at = context.rfind(needle) if needle else -1
        if at >= 0:
            return context.count("\n", at)
    return None


def run(out, paths):
    with open(out, "w") as sink:
        for path in paths:
            for stamp, user, action, context, turn in moments(path):
                ids, ms = recall(f"{user}\n{action}")
                began = time.perf_counter()
                lines = openers(ids)
                where = {rid: in_context(rid, lines.get(rid, ""), context) for rid in ids}
                peek = (time.perf_counter() - began) * 1000
                sink.write(json.dumps({"file": os.path.basename(path), "at": stamp, "turn": turn,
                                       "search": bool(SEARCH.search(action)), "ids": ids,
                                       "where": where, "recall_ms": ms, "peek_ms": peek}) + "\n")


def score(out):
    rows = [json.loads(line) for line in open(out)]
    print("moments", len(rows))
    ms = [r["recall_ms"] for r in rows]
    print(f"recall ms p50 {statistics.median(ms):.0f} p95 {sorted(ms)[int(len(ms) * .95)]:.0f}")
    print(f"context peek ms p50 {statistics.median(r['peek_ms'] for r in rows):.1f}")
    some_in = sum(any(v is not None for v in r["where"].values()) for r in rows)
    some_out = sum(any(v is None for v in r["where"].values()) for r in rows)
    print(f"a top-3 already in context {some_in / len(rows):.0%}; a top-3 not in context {some_out / len(rows):.0%}")
    print(f"recall returned nothing {sum(not r['ids'] for r in rows) / len(rows):.0%}")
    after = [r for r in rows if r["search"] and r["at"] >= "2026-11-05T03:2"]
    hit = [r for r in after if SYM in r["ids"]]
    held = [r for r in after if r["where"].get(SYM) is not None]
    print(f"search moments after the sym rule: {len(after)}; recall returned it {len(hit)}; it was in context {len(held)}")
    back = [r["where"][SYM] for r in held]
    if back:
        print("lines back where it sat:", sorted(back)[:10])


if __name__ == "__main__":
    if sys.argv[1] == "run":
        run(sys.argv[2], sys.argv[3:])
    else:
        score(sys.argv[2])
