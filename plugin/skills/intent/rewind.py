#!/usr/bin/env python3
"""Rewind: pull the team's own words out of Claude Code session transcripts, for /intent's Gather step.

Usage: rewind.py <candidates.md> <transcript.jsonl> [<transcript.jsonl> ...] [--since YYYY-MM-DD] [--exclude word,word]

Writes every message the team wrote -- what the operator typed and what the repo agent said back --
numbered and in time order, with its timestamp, to the candidates file. The words file is built from
the candidates, without the numbers and timestamps. Nothing records who said a message. Tool calls,
tool results, the agent's thinking, hook and system text and compaction summaries are left out.
--exclude drops messages containing any of the listed words (for threads the operator keeps out).
"""
import json, re, sys

args = sys.argv[1:]
out, paths, since, exclude = args[0], [], None, []
i = 1
while i < len(args):
    if args[i] == "--since": since = args[i + 1]; i += 2
    elif args[i] == "--exclude": exclude = [w.strip() for w in args[i + 1].split(",") if w.strip()]; i += 2
    else: paths.append(args[i]); i += 1
drop = re.compile(r"\b(" + "|".join(re.escape(w) for w in exclude) + r")\b", re.I) if exclude else None

msgs = []
for path in paths:
    for line in open(path):
        try: r = json.loads(line)
        except json.JSONDecodeError: continue
        kind = r.get("type")
        if kind not in ("user", "assistant") or r.get("isMeta") or r.get("isCompactSummary"): continue
        c = r.get("message", {}).get("content")
        if isinstance(c, list):
            if any(x.get("type") == "tool_result" for x in c): continue
            text = " ".join(x.get("text", "") for x in c if x.get("type") == "text")
        else:
            text = c or ""
        text = text.strip()
        ts = r.get("timestamp", "")
        if (not text or text.startswith("<") or text.startswith("This session is being continued")
                or text.startswith("Caveat:") or (since and ts[:10] < since) or (drop and drop.search(text))):
            continue
        msgs.append((ts, text))

msgs.sort(key=lambda m: m[0])
with open(out, "w") as f:
    for n, (ts, text) in enumerate(msgs):
        f.write(f"[{n}] {ts[:16]} {text}\n\n")
print(f"{len(msgs)} messages -> {out}")
