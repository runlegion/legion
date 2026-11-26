#!/usr/bin/env python3
"""Sweep: rewrite decision-log hedges in the legion document store as the rule and its reason.

Usage:
  sweep.py propose <workdir> [doc_id ...]   find every hedged paragraph, ask Haiku 5.5 for a rewrite
  sweep.py review  <workdir>                print each proposal as a word-level diff, flags first
  sweep.py apply   <workdir> [--write]      revise each document (dry run without --write)

A hedge is "Current choice (date, who): X; revisable", "Approved date (#n); revisable" and the
like. propose sends Haiku one paragraph at a time and splices the answer back into the JSON
itself, so criterion ids and document structure never pass through the model. Fields that exist
to hold history are never touched. apply re-reads each document and refuses a paragraph that has
changed since the proposal, so a rerun never writes twice. Nothing is written without --write.
"""
import concurrent.futures, difflib, json, os, re, subprocess, sys
from collections import defaultdict

HERE = os.path.dirname(os.path.abspath(__file__))
PROMPT = os.path.join(HERE, "prompt.md")
MODEL = "claude-haiku-5-5"  # the Agent alias "haiku" still runs Haiku 4.5
HEDGE = re.compile(r"current choice|revisable|\bapproved 20\d\d", re.I)
IDS = re.compile(r"\bN?FR-[A-Z0-9-]+-\d+\b|\brev \d+\b")
DATE = re.compile(r"\b20\d\d-\d\d-\d\d\b")
NUM = re.compile(r"\d+(?:[.,]\d+)?")
# Fields whose job is to hold history or provenance: left as written.
HISTORY = {"stickies", "provenance", "answers", "answered", "traces_to", "from"}


def legion(*args):
    return subprocess.run(["legion", *args], capture_output=True, text=True, check=True).stdout


def payload_of(doc):
    payload = doc["payload"]
    return json.loads(payload) if isinstance(payload, str) else payload


def leaves(node, path=()):
    if isinstance(node, str):
        yield path, node
    elif isinstance(node, dict):
        for key, value in node.items():
            yield from leaves(value, path + (key,))
    elif isinstance(node, list):
        for i, value in enumerate(node):
            yield from leaves(value, path + (i,))


def units(doc):
    for path, text in leaves(payload_of(doc)):
        if any(key in HISTORY for key in path if isinstance(key, str)) or not HEDGE.search(text):
            continue
        for i, para in enumerate(text.split("\n\n")):
            if HEDGE.search(para):
                yield {"doc": doc["id"], "surface": doc.get("surface"), "doc_type": doc["doc_type"],
                       "path": list(path), "para": i, "before": para}


def numbers(text):
    return sorted(NUM.findall(DATE.sub("", re.sub(r"#\d+|\b01a[0-9a-f-]+", "", text))))


def check(before, after):
    problems = []
    left = len(HEDGE.findall(after))
    if left >= len(HEDGE.findall(before)):
        problems.append("no hedge removed")
    elif left:
        problems.append(f"{left} hedge kept, check it")
    lost = set(IDS.findall(before)) - set(IDS.findall(after))
    if lost:
        problems.append(f"ids lost: {sorted(lost)}")
    if numbers(before) != numbers(after):
        problems.append("numbers changed")
    if not after.strip():
        problems.append("empty")
    return problems


def ask(unit):
    message = (f"Record {unit['doc']} ({unit['doc_type']}), field "
               f"{'.'.join(map(str, unit['path']))}. Paragraph:\n\n{unit['before']}")
    out = subprocess.run(
        ["claude", "-p", "--model", MODEL, "--system-prompt-file", PROMPT, "--tools", "",
         "--output-format", "json", "--no-session-persistence", "--strict-mcp-config",
         "--setting-sources", ""],
        input=message, capture_output=True, text=True, timeout=300)
    try:
        envelope = json.loads(out.stdout)
        result = envelope["result"].strip()
        answer = json.loads(result[result.index("{"):result.rindex("}") + 1])
    except (ValueError, KeyError) as error:
        return {**unit, "action": "error", "why": f"{error}: {out.stdout[:200]} {out.stderr[:200]}"}
    unit.update(action=answer["action"], why=answer.get("why", ""), cost=envelope.get("total_cost_usd"))
    if answer["action"] == "rewrite":
        unit["after"] = answer["text"]
        unit["problems"] = check(unit["before"], answer["text"])
    return unit


def propose(workdir, only):
    docs = json.loads(legion("document", "list", "--json"))
    todo = [u for d in docs if not only or d["id"] in only for u in units(d)]
    print(f"{len(todo)} hedged paragraphs", file=sys.stderr)
    with open(os.path.join(workdir, "proposals.jsonl"), "w") as out, \
            concurrent.futures.ThreadPoolExecutor(8) as pool:
        for n, result in enumerate(pool.map(ask, todo), 1):
            out.write(json.dumps(result) + "\n")
            out.flush()
            print(f"{n}/{len(todo)} {result['doc']} {result['action']}", file=sys.stderr)


def proposals(workdir):
    return [json.loads(line) for line in open(os.path.join(workdir, "proposals.jsonl"))]


def review(workdir):
    found = proposals(workdir)
    found.sort(key=lambda u: (not u.get("problems") and u["action"] != "error", u["doc"]))
    cost = sum(u.get("cost") or 0 for u in found)
    print(f"{len(found)} paragraphs, {sum(u['action'] == 'rewrite' for u in found)} rewrites, "
          f"{sum(bool(u.get('problems')) for u in found)} flagged, ${cost:.2f}\n")
    for u in found:
        print(f"== {u['doc']} {'.'.join(map(str, u['path']))} [{u['action']}] "
              f"{u.get('problems') or ''} {u['why']}")
        if u.get("after"):
            a, b = re.findall(r"\S+|\s+", u["before"]), re.findall(r"\S+|\s+", u["after"])
            for op, i1, i2, j1, j2 in difflib.SequenceMatcher(None, a, b, autojunk=False).get_opcodes():
                if op != "equal":
                    print(f"  - {''.join(a[i1:i2]).strip()!r}\n  + {''.join(b[j1:j2]).strip()!r}")


def walk(node, path):
    for key in path:
        node = node[key]
    return node


def apply(workdir, write):
    by_doc = defaultdict(list)
    for u in proposals(workdir):
        if u["action"] == "rewrite" and "no hedge removed" not in u.get("problems", []):
            by_doc[u["doc"]].append(u)
    os.makedirs(os.path.join(workdir, "payloads"), exist_ok=True)
    for doc_id, found in sorted(by_doc.items()):
        payload = payload_of(json.loads(legion("document", "view", doc_id, "--json")))
        stale = 0
        for u in found:
            paras = walk(payload, u["path"]).split("\n\n")
            if paras[u["para"]] != u["before"]:
                stale += 1
                continue
            paras[u["para"]] = u["after"]
            walk(payload, u["path"][:-1])[u["path"][-1]] = "\n\n".join(paras)
        if stale:
            print(f"{doc_id}: REFUSED, {stale} paragraph(s) changed since the proposal")
            continue
        target = os.path.join(workdir, "payloads", f"{doc_id}.json")
        with open(target, "w") as out:
            json.dump(payload, out, indent=1, ensure_ascii=False)
        if write:
            result = subprocess.run(["legion", "document", "revise", doc_id, "--from", target],
                                    capture_output=True, text=True)
            if result.returncode:
                print(f"{doc_id}: FAILED, {result.stderr.strip()}")
                continue
        print(f"{doc_id}: {len(found)} paragraph(s) {'revised' if write else 'ready'}")


def main():
    if len(sys.argv) < 3 or sys.argv[1] not in ("propose", "review", "apply"):
        sys.exit(__doc__)
    step, workdir, rest = sys.argv[1], sys.argv[2], sys.argv[3:]
    os.makedirs(workdir, exist_ok=True)
    if step == "propose":
        propose(workdir, set(rest))
    elif step == "review":
        review(workdir)
    else:
        apply(workdir, "--write" in rest)


if __name__ == "__main__":
    main()
