"""Switch Syndeo's `current` link over and over while commands start through it.

    python3 -I ci/switch-stress.py --private DIR --command PATH --versions A B
        (--expect-output TEMPLATE | --expect-doctor)
        [--renames N] [--min-seconds S]
        [--as-user USER] [--env KEY=VALUE]... [--settle-lookups N] [--settle-launches N]

What the macOS package promises of the switch, checked as stated:
- a lookup through `current`, or a command started through it, either fails
  with ENOENT or EINVAL, or succeeds with all of one complete version;
- it fails only while the switch is happening;
- once switching stops, every lookup and every start succeeds, as the version
  `current` names.

The switching is the postinstall's own: a new symlink, then rename(2) over
`current` (what `mv -h` does), between two complete version trees. That is
the switch alone; a whole upgrade, which also removes the old tree, is
ci/upgrade-stress.py's.

A lookup opens `current/syndeo` once. Only that open may fail, and only with
ENOENT or EINVAL while switching. Which version it found is judged from that
one descriptor alone: the path the kernel gives for it (F_GETPATH), its
device and inode, and its bytes when the versions' bytes differ. All of them
have to name the same version; nothing is looked up a second time.

Starts run COMMAND, the command link in bin/. A start counts as the transient
failure only when exec, or the shell or env opening it, reported "No such file
or directory" or "Invalid argument" for COMMAND itself. Anything else that
fails, and any success that is not wholly one version, is a rejection.

How often the transient failure happened is printed, and is not judged.
Exits 1 on any rejection, 0 otherwise.
"""
import argparse
import errno
import fcntl
import os
import subprocess
import sys
import threading
import time

TRANSIENT = {errno.ENOENT: "ENOENT", errno.EINVAL: "EINVAL"}

# <sys/fcntl.h>: the path of the file a descriptor is open on, as the kernel
# names it, and the same without firmlinks (/System/Volumes/Data/...).
F_GETPATH = getattr(fcntl, "F_GETPATH", 50)
F_GETPATH_NOFIRMLINK = 102
MAXPATHLEN = 1024


def fd_path(fd, command=F_GETPATH):
    return os.fsdecode(fcntl.fcntl(fd, command, bytes(MAXPATHLEN)).split(b"\0", 1)[0])


def fd_read(fd):
    chunks = []
    while True:
        chunk = os.read(fd, 1 << 20)
        if not chunk:
            return b"".join(chunks)
        chunks.append(chunk)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--private", required=True)
    ap.add_argument("--command", required=True)
    ap.add_argument("--versions", nargs=2, required=True)
    ap.add_argument("--expect-output")
    ap.add_argument("--expect-doctor", action="store_true")
    ap.add_argument("--renames", type=int, default=10000)
    ap.add_argument("--min-seconds", type=float, default=3.0)
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
    if len(set(versions)) != 2:
        ap.error("two different versions")
    # Directories by device and inode: the kernel may name /usr/local as
    # /System/Volumes/Data/usr/local, and both are the same directory.
    tree_ids = {}
    for v in versions:
        st = os.stat(trees[v])
        tree_ids[(st.st_dev, st.st_ino)] = v
    # Each version's syndeo, opened directly: the names the kernel gives that
    # descriptor, the file's device and inode, and its bytes. A lookup through
    # `current` is judged against these.
    by_name, by_id, contents = {}, {}, {}
    for v in versions:
        fd = os.open(os.path.join(trees[v], "syndeo"), os.O_RDONLY | os.O_CLOEXEC)
        try:
            for name in (fd_path(fd), fd_path(fd, F_GETPATH_NOFIRMLINK)):
                by_name[name] = v
            st = os.fstat(fd)
            by_id[(st.st_dev, st.st_ino)] = v
            contents[v] = fd_read(fd)
        finally:
            os.close(fd)
    if len(set(by_name.values())) != 2 or len(by_id) != 2:
        ap.error("the two versions' syndeo are not two different files")
    by_body = {body: v for v, body in contents.items()}
    if len(by_body) != 2:
        # Copies of one build: the bytes cannot tell the trees apart, so a
        # lookup is judged by its name and its device and inode alone.
        by_body = None

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

    through = os.path.join(current, "syndeo")

    def lookup():
        during = state["switching"]
        try:
            fd = os.open(through, os.O_RDONLY | os.O_CLOEXEC)
        except OSError as e:
            if e.errno in TRANSIENT:
                transient("lookup " + TRANSIENT[e.errno], during)
            else:
                reject("lookup failed: %s" % e)
            return False
        # The open found a file. Everything below is about that file, from
        # this descriptor; none of it may fail.
        try:
            name = fd_path(fd)
            st = os.fstat(fd)
            body = fd_read(fd) if by_body is not None else None
        except OSError as e:
            reject("current/syndeo was opened, but its descriptor could not be read: %s" % e)
            return False
        finally:
            os.close(fd)
        found = {by_name.get(name), by_id.get((st.st_dev, st.st_ino))}
        if by_body is not None:
            found.add(by_body.get(body))
        if len(found) != 1 or None in found:
            reject("current/syndeo opened %s (inode %d), which is not wholly one version" % (name, st.st_ino))
            return False
        v = found.pop()
        if not during and v != state["final"]:
            reject("current/syndeo opened %s's syndeo after switching stopped at %s" % (v, state["final"]))
            return False
        count("lookup ok %s" % v)
        return True

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
            return False
        if r.returncode != 0:
            reasons = [m for m in ("No such file or directory", "Invalid argument")
                       if ("%s: %s" % (a.command, m)) in r.stderr]
            if reasons and r.returncode in (2, 126, 127):
                transient("start %s" % ("ENOENT" if reasons[0].startswith("No") else "EINVAL"), during)
            else:
                reject("start exited %d: %s" % (r.returncode, r.stderr.strip()[:200]))
            return False
        version, problem = judge_output(r.stdout)
        if problem:
            reject("start succeeded but %s" % problem)
            return False
        if not during and version != state["final"]:
            reject("a start after switching stopped ran %s, not %s" % (version, state["final"]))
            return False
        count("start ok %s" % version)
        return True

    def loop(fn, *fn_args):
        while state["switching"]:
            fn(*fn_args)

    threads = [threading.Thread(target=loop, args=(lookup,)) for _ in range(2)]
    threads += [threading.Thread(target=loop, args=(start, "start%d" % i)) for i in range(2)]
    for t in threads:
        t.start()

    started = time.monotonic()
    switches = 0
    last = None
    try:
        while switches < a.renames or time.monotonic() - started < a.min_seconds:
            target = versions[(switches + 1) % 2]
            if os.path.lexists(pending):
                os.unlink(pending)
            os.symlink(target, pending)
            os.rename(pending, current)
            last = target
            switches += 1
    finally:
        state["switching"] = False
        for t in threads:
            t.join()

    final = os.readlink(current)
    if final != last:
        reject("current ends at %s, but the last switch was to %s" % (final, last))
    state["final"] = final
    # Settled: every lookup and every start succeeds, as the final version.
    looked = sum(1 for _ in range(a.settle_lookups) if lookup())
    if looked != a.settle_lookups:
        reject("%d of %d lookups after switching stopped did not succeed" % (a.settle_lookups - looked, a.settle_lookups))
    settled = sum(1 for _ in range(a.settle_launches) if start("settle"))
    if settled != a.settle_launches:
        reject("%d of %d starts after switching stopped did not succeed" % (a.settle_launches - settled, a.settle_launches))

    print("switches: %d renames; current ends at %s" % (switches, final))
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
