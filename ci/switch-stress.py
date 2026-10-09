"""Switch Syndeo's `current` link over and over while commands start through it.

    python3 -I ci/switch-stress.py --private DIR --command PATH --versions A B
        (--expect-output TEMPLATE | --expect-doctor)
        [--renames N] [--min-seconds S]
        [--postinstall VERSION SCRIPT]... [--postinstalls N]
        [--as-user USER] [--env KEY=VALUE]... [--settle-lookups N] [--settle-launches N]

What the macOS package promises of the switch, checked as stated:
- a lookup through `current`, or a command started through it, either fails
  with ENOENT or EINVAL, or succeeds with all of one complete version;
- it fails only while the switch is happening;
- once switching stops, every lookup and every start succeeds, as the version
  `current` names.

The switching is the postinstall's own: a new symlink, then rename(2) over
`current` (what `mv -h` does). --postinstall runs real postinstall scripts too.
Lookups read `current` and the `syndeo` file through it; starts run COMMAND,
the command link in bin/. A start counts as the transient failure only when
exec, or the shell or env opening it, reported "No such file or directory" or
"Invalid argument" for COMMAND itself. Anything else that fails, and any
success that is not wholly one version, is a rejection.

How often the transient failure happened is printed, and is not judged.
Exits 1 on any rejection, 0 otherwise.
"""
import argparse
import errno
import os
import subprocess
import sys
import threading
import time

TRANSIENT = {errno.ENOENT: "ENOENT", errno.EINVAL: "EINVAL"}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--private", required=True)
    ap.add_argument("--command", required=True)
    ap.add_argument("--versions", nargs=2, required=True)
    ap.add_argument("--expect-output")
    ap.add_argument("--expect-doctor", action="store_true")
    ap.add_argument("--renames", type=int, default=10000)
    ap.add_argument("--min-seconds", type=float, default=3.0)
    ap.add_argument("--postinstall", nargs=2, action="append", default=[], metavar=("VERSION", "SCRIPT"))
    ap.add_argument("--postinstalls", type=int, default=0)
    ap.add_argument("--as-user")
    ap.add_argument("--env", action="append", default=[],
                    help="KEY=VALUE for each start; {thread} in VALUE becomes the starting thread's name")
    ap.add_argument("--settle-lookups", type=int, default=300)
    ap.add_argument("--settle-launches", type=int, default=50)
    a = ap.parse_args()
    if bool(a.expect_output) == a.expect_doctor:
        ap.error("exactly one of --expect-output and --expect-doctor")

    current = os.path.join(a.private, "current")
    pending = os.path.join(a.private, ".current.new")
    versions = list(a.versions)
    trees = {v: os.path.join(a.private, v) for v in versions}
    real_trees = {os.path.realpath(t) for t in trees.values()}
    # Directories by device and inode: the kernel may name /usr/local as
    # /System/Volumes/Data/usr/local, and both are the same directory.
    tree_ids = {}
    for v in versions:
        st = os.stat(trees[v])
        tree_ids[(st.st_dev, st.st_ino)] = v
    contents = {}
    for v in versions:
        with open(os.path.join(trees[v], "syndeo"), "rb") as f:
            contents[v] = f.read()
    if contents[versions[0]] == contents[versions[1]]:
        # Copies of one build: the bytes cannot tell the trees apart, so a
        # lookup is judged by which tree it resolved to instead.
        contents = None

    args = ["doctor"] if a.expect_doctor else ["--version"]

    def launch_for(thread):
        env = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "HOME": os.environ.get("HOME", "/")}
        for kv in a.env:
            key, _, value = kv.partition("=")
            env[key] = value.replace("{thread}", thread)
        if a.as_user:
            env["HOME"] = os.path.expanduser("~" + a.as_user)
            command = ["/usr/bin/sudo", "-u", a.as_user, "-H", "/usr/bin/env", "-i"]
            command += ["%s=%s" % kv for kv in env.items()] + [a.command] + args
            return command, None
        return [a.command] + args, env

    state = {"switching": True, "final": None}
    lock = threading.Lock()
    tally = {}
    rejects = []

    def count(key):
        with lock:
            tally[key] = tally.get(key, 0) + 1

    def reject(what):
        with lock:
            rejects.append(what)

    def transient(name, during):
        if during:
            count("transient %s" % name)
        else:
            reject("%s after switching had stopped" % name)

    def lookup():
        during = state["switching"]
        try:
            target = os.readlink(current)
            if target not in versions:
                reject("current read as %r" % target)
                return
            if not during and target != state["final"]:
                reject("current read as %s after switching stopped at %s" % (target, state["final"]))
                return
            path = os.path.join(current, "syndeo")
            with open(path, "rb") as f:
                body = f.read()
            if contents is not None:
                if body not in contents.values():
                    reject("current/syndeo read as neither version's file")
                    return
            real = os.path.realpath(path)
            if os.path.dirname(real) not in real_trees:
                reject("current/syndeo resolved to %s" % real)
                return
            count("lookup ok")
        except OSError as e:
            if e.errno in TRANSIENT:
                transient("lookup " + TRANSIENT[e.errno], during)
            else:
                reject("lookup failed: %s" % e)

    def judge_output(out):
        if a.expect_doctor:
            dirs = set()
            for line in out.splitlines():
                parts = line.split()
                if len(parts) == 2 and parts[0] in ("syndeo-net", "syndeo-keystore", "syndeo-agent"):
                    dirs.add(os.path.dirname(parts[1]))
            if len(dirs) != 1:
                return None, "doctor's siblings came from %s" % (sorted(dirs) or "nowhere")
            d = dirs.pop()
            try:
                st = os.stat(d)
            except OSError as e:
                return None, "doctor's siblings came from %s, which cannot be read: %s" % (d, e)
            v = tree_ids.get((st.st_dev, st.st_ino))
            if v is None:
                return None, "doctor's siblings came from %s, neither version" % d
            return v, None
        first = out.splitlines()[0] if out else ""
        for v in versions:
            if first == a.expect_output.format(version=v):
                return v, None
        return None, "output %r is neither version's" % first

    def start(thread):
        during = state["switching"]
        launch, env = launch_for(thread)
        try:
            r = subprocess.run(launch, env=env, capture_output=True, text=True, timeout=120)
        except OSError as e:
            if e.errno in TRANSIENT:
                transient("start " + TRANSIENT[e.errno], during)
            else:
                reject("start failed: %s" % e)
            return
        if r.returncode != 0:
            reasons = [m for m in ("No such file or directory", "Invalid argument")
                       if ("%s: %s" % (a.command, m)) in r.stderr]
            if reasons and r.returncode in (2, 126, 127):
                transient("start %s" % ("ENOENT" if reasons[0].startswith("No") else "EINVAL"), during)
            else:
                reject("start exited %d: %s" % (r.returncode, r.stderr.strip()[:200]))
            return
        version, problem = judge_output(r.stdout)
        if problem:
            reject("start succeeded but %s" % problem)
        elif not during and version != state["final"]:
            reject("a start after switching stopped ran %s, not %s" % (version, state["final"]))
        else:
            count("start ok %s" % version)

    def loop(fn, *fn_args):
        while state["switching"]:
            fn(*fn_args)

    threads = [threading.Thread(target=loop, args=(lookup,)) for _ in range(2)]
    threads += [threading.Thread(target=loop, args=(start, "start%d" % i)) for i in range(2)]
    for t in threads:
        t.start()

    started = time.monotonic()
    switches = 0
    try:
        while switches < a.renames or time.monotonic() - started < a.min_seconds:
            target = versions[(switches + 1) % 2]
            if os.path.lexists(pending):
                os.unlink(pending)
            os.symlink(target, pending)
            os.rename(pending, current)
            switches += 1
        runs = 0
        for i in range(a.postinstalls if a.postinstall else 0):
            v, script = a.postinstall[i % len(a.postinstall)]
            r = subprocess.run(["/bin/sh", script, "stress", "/", "/", "/"], capture_output=True, text=True)
            if r.returncode != 0:
                reject("postinstall %s exited %d: %s" % (v, r.returncode, r.stdout.strip()[-200:]))
            elif os.readlink(current) != v:
                reject("postinstall %s returned with current at %s" % (v, os.readlink(current)))
            runs += 1
    finally:
        state["switching"] = False
        for t in threads:
            t.join()

    final = os.readlink(current)
    state["final"] = final
    for _ in range(a.settle_lookups):
        lookup()
    settled_before = sum(n for k, n in tally.items() if k.startswith("start ok"))
    for _ in range(a.settle_launches):
        start("settle")
    settled = sum(n for k, n in tally.items() if k.startswith("start ok")) - settled_before
    if settled != a.settle_launches:
        reject("%d of %d starts after switching stopped did not succeed" % (a.settle_launches - settled, a.settle_launches))

    print("switches: %d renames, %d postinstalls; current ends at %s" % (switches, runs, final))
    for key in sorted(tally):
        print("%-28s %d" % (key, tally[key]))
    transients = sum(n for k, n in tally.items() if k.startswith("transient"))
    print("transient failures while switching (recorded, not judged): %d" % transients)
    if rejects:
        print("REJECTED: %d" % len(rejects))
        seen = {}
        for r in rejects:
            seen[r] = seen.get(r, 0) + 1
        for r, n in sorted(seen.items(), key=lambda x: -x[1])[:10]:
            print("  %4d  %s" % (n, r))
        return 1
    print("no rejections")
    return 0


if __name__ == "__main__":
    sys.exit(main())
