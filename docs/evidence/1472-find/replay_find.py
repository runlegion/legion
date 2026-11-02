#!/usr/bin/env python3
"""Replay the find invocations of permission-denied Bash commands through the built `legion find`.

Input: denied.json from extract_denied.py. Output: counts by group and outcome.
"""
import collections, fnmatch, json, os, shlex, subprocess, sys

rows = json.load(open(sys.argv[1]))
bin_path = sys.argv[2]
OPS = {";", "&&", "||", "|", "&", "|&", ";;", "(", ")", "\n", "$("}
REDIR = {">", ">>", "<", "2>", "2>>", "&>", ">&", "<&"}
LAYOUT = ("node_modules", "dist", "build", "target", ".pnpm", ".wrangler", ".git")


def invocations(cmd):
    """argv lists following each `find` at a command position. None if it does not tokenize."""
    lex = shlex.shlex(cmd, posix=True, punctuation_chars=";&|()<>")
    lex.whitespace = " \t\r"
    lex.whitespace_split = True
    lex.commenters = ""
    try:
        toks = list(lex)
    except ValueError:
        return None
    out, at_start, i = [], True, 0
    cwd = None
    env = {}
    while i < len(toks):
        t = toks[i]
        if t == "\n" or set(t) <= set(";&|()"):
            at_start = True
            i += 1
            continue
        if at_start and "=" in t and not t.startswith(("-", "=")) and t.split("=", 1)[0].isidentifier():
            name, value = t.split("=", 1)
            env[name] = os.path.expandvars(value)
            for k, v in env.items():
                env[name] = env[name].replace("$" + k, v)
            i += 1
            continue
        if at_start and t == "cd" and i + 1 < len(toks):
            cwd = os.path.expanduser(toks[i + 1]) if cwd is None else os.path.join(cwd, os.path.expanduser(toks[i + 1]))
            i += 2
            at_start = False
            continue
        if at_start and t in ("find", "$(find"):
            argv, i = [], i + 1
            discards = False
            while i < len(toks):
                w = toks[i]
                if w == "\n":
                    break
                if set(w) <= set(";&|") and w != ";":
                    break
                if w == ";" and argv and argv[-1] != "{}":
                    break
                if w == ")" and argv.count("(") <= argv.count(")"):
                    break
                fd = None
                if w in ("2", "1") and i + 1 < len(toks) and set(toks[i + 1]) <= set("<>&"):
                    fd = w
                    i += 1
                    w = toks[i]
                if set(w) <= set("<>&") and w:
                    # A redirect: drop it and its target, noting a discarded stderr.
                    target = toks[i + 1] if i + 1 < len(toks) else ""
                    if target == "/dev/null" and (fd == "2" or w == "&>"):
                        discards = True
                    i += 2
                    continue
                argv.append(w)
                i += 1
            for k, v in sorted(env.items(), key=lambda kv: -len(kv[0])):
                argv = [w.replace("${" + k + "}", v).replace("$" + k, v) for w in argv]
            out.append((cwd, argv, discards))
            at_start = False
            continue
        at_start = t.endswith("=") or (at_start and "=" in t and not t.startswith("-")) or t in ("then", "do", "else", "$", "time", "!", "{")
        i += 1
    return out


def operands(argv):
    ops = []
    for w in argv:
        if w.startswith("-") or w in ("(", "!"):
            break
        ops.append(w)
    return ops or ["."]


def group(argv, refusal):
    text = " ".join(argv)
    ops = operands(argv)
    if "outside every watched repo" in refusal:
        return "outside a repo"
    if any(any(p in o.split("/") for p in LAYOUT) for o in ops) or any(
        w in ("-path", "-ipath") and any(p in argv[i + 1] for p in LAYOUT) and (i == 0 or argv[i - 1] not in ("-not", "!"))
        for i, w in enumerate(argv[:-1])
    ):
        return "repo or build layout"
    if "-maxdepth" in argv and ("-name" in argv or "-iname" in argv or "-type" in argv) and ("-type" in argv and "d" in argv):
        return "repo or build layout"
    if any(w in argv for w in ("-name", "-iname")):
        return "files by name or extension"
    if all(w in ("-type", "f", "d", "-maxdepth", "-mindepth", "-print", "-not", "-path") or not w.startswith("-") for w in argv):
        return "listing a tree"
    return "other"


def tags(argv, refusal):
    text = " ".join(argv)
    ops = " ".join(operands(argv))
    out = []
    # Named as what is being looked in or for, not excluded.
    positive = [w for i, w in enumerate(argv) if not (i and (argv[i - 1] in ("-path", "-ipath") and i > 1 and argv[i - 2] in ("-not", "!")))]
    ptext = " ".join(positive)
    if "dist" in ptext.split("/") or "/dist" in ptext or ptext.startswith("dist") or " dist" in ptext:
        out.append("dist")
    if "node_modules" in ptext:
        out.append("node_modules")
    if "outside every watched repo" in refusal:
        out.append("outside a watched repo")
    if "linked worktree" in refusal:
        out.append("in a linked worktree")
    return out


def parse_expr(words):
    """find's grammar for the predicates `legion find` accepts, over os.walk paths."""
    pos = [0]
    limits = {"max": None, "min": 0, "explicit": False, "printed": False, "prune": False}

    def peek():
        return words[pos[0]] if pos[0] < len(words) else None

    def or_():
        left = and_()
        while peek() in ("-o", "-or"):
            pos[0] += 1
            l, r = left, and_()
            left = lambda p, d, l=l, r=r: l(p, d) or r(p, d)
        return left

    def and_():
        left = not_()
        while peek() not in (None, "-o", "-or", ")"):
            if peek() in ("-a", "-and"):
                pos[0] += 1
            l, r = left, not_()
            left = lambda p, d, l=l, r=r: l(p, d) and r(p, d)
        return left

    def not_():
        if peek() in ("!", "-not"):
            pos[0] += 1
            inner = not_()
            return lambda p, d: not inner(p, d)
        return primary()

    def primary():
        w = peek()
        pos[0] += 1
        if w == "(":
            inner = or_()
            pos[0] += 1
            return inner
        if w == "-print":
            limits["explicit"] = True

            def printing(p, d):
                limits["printed"] = True
                return True

            return printing
        if w == "-prune":

            def pruning(p, d):
                limits["prune"] = True
                return True

            return pruning
        v = words[pos[0]]
        pos[0] += 1
        if w in ("-name", "-iname"):
            base = lambda p: os.path.basename(p.rstrip("/")) or p
            if w == "-iname":
                return lambda p, d: fnmatch.fnmatchcase(base(p).lower(), v.lower())
            return lambda p, d: fnmatch.fnmatchcase(base(p), v)
        if w in ("-path", "-ipath"):
            if w == "-ipath":
                return lambda p, d: fnmatch.fnmatchcase(p.lower(), v.lower())
            return lambda p, d: fnmatch.fnmatchcase(p, v)
        if w == "-type":
            return lambda p, d: d == v
        if w == "-maxdepth":
            limits["max"] = int(v)
        if w == "-mindepth":
            limits["min"] = int(v)
        return lambda p, d: True

    expr = or_() if words else (lambda p, d: True)
    return expr, limits


def on_disk(argv, cwd):
    """What real find prints for an accepted form, walked in Python, skipping
    nothing, with the walk's own count of the part in .git."""
    ops = operands(argv)
    expr, limits = parse_expr(argv[len([w for w in ops if w in argv]) if ops != ["."] or (argv and argv[0] == ".") else 0:])
    out = set()

    def judge(path, kind):
        limits["printed"] = limits["prune"] = False
        answer = expr(path, kind)
        if limits["printed"] if limits["explicit"] else answer:
            out.add(path)
        return limits["prune"]

    for op in ops:
        base = os.path.join(cwd, op)
        for root, dirs, files in os.walk(base):
            rel = os.path.relpath(root, base)
            depth = 0 if rel == "." else rel.count("/") + 1
            shown = op if rel == "." else (op.rstrip("/") + "/" + rel)
            if depth >= limits["min"] and (limits["max"] is None or depth <= limits["max"]):
                if judge(shown, "d"):
                    dirs[:] = []
                    files = []
            if limits["max"] is not None and depth >= limits["max"]:
                dirs[:] = []
            if depth + 1 >= limits["min"] and (limits["max"] is None or depth + 1 <= limits["max"]):
                for f in files:
                    judge(shown.rstrip("/") + "/" + f, "f")
    return out


def inferred(cwd):
    """For a cwd gone from disk: the refusal its path would get. Inferred, not replayed."""
    if "/.claude/worktrees/" in cwd or "-wt/" in cwd or cwd.rstrip("/").split("/")[-1].startswith("agent-"):
        return "inferred: worktree"
    if cwd.startswith(("/Volumes/store/tmp", "/tmp", "/private/tmp")):
        return "inferred: outside"
    return "inferred: unknown"


results = collections.Counter()
as_typed = collections.defaultdict(collections.Counter)
typed_discard = 0
by_group = collections.defaultdict(collections.Counter)
by_tag = collections.defaultdict(collections.Counter)
samples = collections.defaultdict(list)
n_cmd = untok = 0
for row in rows:
    n_cmd += 1
    invs = invocations(row["cmd"])
    if invs is None:
        untok += 1
        by_group["(would not tokenize)"]["unreplayed"] += 1
        continue
    if not invs:
        by_group["(find is not a command word here)"]["unreplayed"] += 1
        continue
    for moved, argv, discards in invs:
        typed_discard += discards
        argv = [os.path.expanduser(a) for a in argv]
        cwd = os.path.join(row["cwd"] or "", moved) if moved else (row["cwd"] or "")
        if not os.path.isdir(cwd):
            outcome, g, t = inferred(cwd), "(cwd no longer exists)", []
        elif any("$" in a or "`" in a for a in argv):
            outcome, g, t = "shell variable", "(unexpanded variable)", []
        else:
            p = subprocess.run([bin_path, "find", *argv], cwd=cwd, capture_output=True, text=True, stdin=subprocess.DEVNULL)
            refusal = p.stderr if p.returncode else ""
            real = [l for l in p.stdout.splitlines()]
            if p.returncode == 0 or real:
                disk = on_disk(argv, cwd)
                only_git = {x for x in disk if "/.git/" in x + "/"}
                missed = disk - set(real) - only_git
                extra = set(real) - disk
                kind = "answered" if real else "answered empty"
                if p.returncode != 0:
                    kind = "partly answered"
                    # What a refused operand held is not this answer's to hold.
                warned = "behind the disk" in p.stderr
                if warned:
                    kind += ", warned stale"
                if extra or (missed and p.returncode == 0):
                    outcome = f"{kind}, differs from disk"
                    samples["differs"].append(f"{cwd[-40:]} :: {' '.join(argv)[:90]} | printed {len(real)}, disk {len(disk)}, missed {sorted(missed)[:3]}, extra {sorted(extra)[:3]}")
                else:
                    outcome = f"{kind}, matches disk"
            elif "sym cannot answer" in p.stderr:
                outcome = "refused: predicate"
            elif "linked worktree" in p.stderr:
                outcome = "refused: worktree"
            elif "this asks for a tree" in p.stderr:
                outcome = "refused: names an ignored tree"
            elif "outside every" in p.stderr:
                outcome = "refused: outside"
            elif "no indexed file" in p.stderr:
                outcome = "refused: not indexed"
            elif "No such file" in p.stderr:
                outcome = "no such path"
            else:
                outcome = "refused: other"
            g, t = group(argv, p.stderr), tags(argv, p.stderr)
            # What the agent would have got: with its own stderr discard, a
            # warned short list is a refusal with nothing on stdout.
            typed_out = p
            if discards:
                typed_out = subprocess.run([bin_path, "find", *argv], cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, stdin=subprocess.DEVNULL)
            got = "printed" if typed_out.stdout.strip() else ("empty, exit 0" if typed_out.returncode == 0 else "nothing, exit 1")
            as_typed[g][got] += 1
            for x in t:
                as_typed[x][got] += 1
            samples[outcome].append(cwd[-40:] + " :: " + " ".join(shlex.quote(a) for a in argv)[:110])
        results[outcome] += 1
        by_group[g][outcome] += 1
        for x in t:
            by_tag[x][outcome] += 1

borrowed = sum(1 for r in rows if r.get("cwd_borrowed"))
rewritten = refused_by_router = 0
for row in rows:
    p = subprocess.run([bin_path, "cmd-check", "--", row["cmd"]], capture_output=True, text=True, env={**os.environ, "LEGION_CMD_POLICY": os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))), "plugin/legion-cmd/policy.json")})
    if "replacement:" in p.stdout and "legion find" in p.stdout:
        rewritten += 1
    elif "decision: rewrite" not in p.stdout:
        refused_by_router += 1
print("router: rewrote to legion find", rewritten, "of", len(rows), "; other decision", refused_by_router)
print("cwd taken from the session's other lines:", borrowed, "commands; typed a stderr discard:", typed_discard, "invocations")
print("AS TYPED (replayed rows only)")
for g, c in sorted(as_typed.items(), key=lambda kv: -sum(kv[1].values())):
    print(f"  {g:36} {dict(c)}")
print("denied commands", n_cmd, "untokenized", untok)
print("invocations", sum(results.values()), dict(results))
print()
for name, table in [("GROUP", by_group), ("NAMED", by_tag)]:
    print(name)
    for g, c in sorted(table.items(), key=lambda kv: -sum(kv[1].values())):
        total = sum(c.values())
        ans = sum(v for k, v in c.items() if k.startswith(("answered", "partly")))
        print(f"  {g:36} {total:4}  answered {ans:3} ({100*ans/total:3.0f}%)  {dict(c)}")
print()
for k, v in samples.items() if len(sys.argv) > 3 else []:
    print(k, len(v))
    for s in v[:40]:
        print("   ", s)
