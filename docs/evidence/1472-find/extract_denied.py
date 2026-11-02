#!/usr/bin/env python3
"""Pull Bash commands containing `find` that were denied, last 7 days, from Claude Code session logs."""
import json, os, sys, time, re, glob

root = os.path.expanduser("~/.claude/projects")
cutoff = time.time() - 7 * 86400
out = sys.argv[1]
files = [p for p in glob.glob(root + "/**/*.jsonl", recursive=True) if os.path.getmtime(p) >= cutoff]
print("files", len(files))
find_word = re.compile(r"(^|[\s;&|(`$])find\s")
rows = []
results = 0
classifier = 0
for path in files:
    uses = {}
    last_cwd = None
    try:
        fh = open(path, errors="replace")
    except OSError:
        continue
    with fh:
        for line in fh:
            try:
                d = json.loads(line)
            except ValueError:
                continue
            # A tool_use line can lack a cwd (subagent logs); the session's
            # latest one stands in, and the row says so.
            last_cwd = d.get("cwd") or last_cwd
            msg = d.get("message")
            if not isinstance(msg, dict):
                continue
            content = msg.get("content")
            if not isinstance(content, list):
                continue
            for block in content:
                if not isinstance(block, dict):
                    continue
                if block.get("type") == "tool_use" and block.get("name") == "Bash":
                    cmd = (block.get("input") or {}).get("command", "")
                    uses[block.get("id")] = (cmd, d.get("cwd") or last_cwd, d.get("timestamp"), not d.get("cwd"))
                elif block.get("type") == "tool_result":
                    results += 1
                    tid = block.get("tool_use_id")
                    if tid not in uses:
                        continue
                    cmd, cwd, ts, cwd_borrowed = uses[tid]
                    if not find_word.search(cmd):
                        continue
                    text = block.get("content")
                    if isinstance(text, list):
                        text = " ".join(x.get("text", "") for x in text if isinstance(x, dict))
                    text = text or ""
                    # The harness's words for a permissions.deny hit; the whole
                    # command sits between them, so a long one must still count.
                    # An auto-mode classifier denial has other words, and is
                    # counted apart.
                    if text.startswith("Permission to use Bash") and "has been denied" in text:
                        rows.append({"cmd": cmd, "cwd": cwd, "ts": ts, "cwd_borrowed": cwd_borrowed})
                    elif text.startswith("Permission for this action was denied"):
                        classifier += 1
print("tool results", results, "permissions.deny with find", len(rows), "classifier denials with find", classifier)
json.dump(rows, open(out, "w"), indent=0)
