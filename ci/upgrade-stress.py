"""Start Syndeo's commands over and over while Installer upgrades it.

    python3 -I ci/upgrade-stress.py --private DIR --command PATH --from VERSION
        --install VERSION PKG [--install VERSION PKG]...
        --expect-output TEMPLATE --expect-file TEMPLATE
        [--log PREFIX] [--settle-lookups N] [--settle-launches N]
        [--installer PATH]

Run as root, on a disposable runner, with VERSION installed. Each --install
is an upgrade, `installer -pkg PKG -target /`, run in turn. What the package
promises of an upgrade, checked as stated:
- while Installer runs, a lookup through `current`, or a command started
  through it, either fails with ENOENT or EINVAL, because the commands are
  unavailable while the old version is replaced, or succeeds wholly as one of
  the versions involved: never a mixture, never anything else;
- every install succeeds and leaves `current` at its version;
- once the last has finished, every lookup and every start succeeds, as the
  last version.

A lookup opens DIR/current/syndeo once and judges that one descriptor: the
path the kernel gives for it (F_GETPATH) and its bytes have to name the same
version, TEMPLATE-formatted. A start runs COMMAND --version; its first line
has to be --expect-output for one version, and it counts as unavailable only
when exec, or env opening COMMAND, said "No such file or directory" or
"Invalid argument" for COMMAND itself.

How long the commands were unavailable, from the first such failure to the
last, is printed for each install, and not judged. Exits 1 on any rejection.
--installer replaces /usr/sbin/installer, for trying the judging out without
installing anything.
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
F_GETPATH = getattr(fcntl, "F_GETPATH", 50)
F_GETPATH_NOFIRMLINK = 102
MAXPATHLEN = 1024


def fd_path(fd, command=F_GETPATH):
    return os.fsdecode(fcntl.fcntl(fd, command, bytes(MAXPATHLEN)).split(b"\0", 1)[0])


def fd_read(fd):
    chunks = []
    while True:
        chunk = os.read(fd, 1 << 16)
        if not chunk:
            return b"".join(chunks)
        chunks.append(chunk)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--private", required=True)
    ap.add_argument("--command", required=True)
    ap.add_argument("--from", dest="start", required=True)
    ap.add_argument("--install", nargs=2, action="append", required=True, metavar=("VERSION", "PKG"))
    ap.add_argument("--expect-output", required=True)
    ap.add_argument("--expect-file", required=True)
    ap.add_argument("--log")
    ap.add_argument("--installer", default="/usr/sbin/installer")
    ap.add_argument("--settle-lookups", type=int, default=300)
    ap.add_argument("--settle-launches", type=int, default=50)
    a = ap.parse_args()

    versions = [a.start] + [v for v, _ in a.install]
    if len(set(versions)) != len(versions):
        ap.error("every version once")
    current = os.path.join(a.private, "current")
    through = os.path.join(current, "syndeo")
    # The private directory stays through an upgrade; only version trees go.
    # Every name the kernel may give it, so that a version's file is known by
    # name before its tree exists.
    fd = os.open(a.private, os.O_RDONLY | os.O_CLOEXEC)
    try:
        private_names = {fd_path(fd), fd_path(fd, F_GETPATH_NOFIRMLINK)}
    finally:
        os.close(fd)
    by_name = {}
    for v in versions:
        for p in private_names:
            by_name[os.path.join(p, v, "syndeo")] = v
    by_body = {a.expect_file.format(version=v).encode(): v for v in versions}
    by_output = {a.expect_output.format(version=v): v for v in versions}
    if len(by_body) != len(versions) or len(by_output) != len(versions):
        ap.error("the templates have to tell every version apart")

    state = {"during": True, "final": None, "install": 0}
    lock = threading.Lock()
    tally = {}
    rejects = []
    unavailable = {}  # install number -> [first, last, count]

    def count(key):
        with lock:
            tally[key] = tally.get(key, 0) + 1

    def reject(what):
        with lock:
            rejects.append(what)

    def transient(name, during, install):
        if not during:
            reject("%s after the last install had finished" % name)
            return
        now = time.monotonic()
        with lock:
            tally["unavailable " + name] = tally.get("unavailable " + name, 0) + 1
            span = unavailable.setdefault(install, [now, now, 0])
            span[1] = now
            span[2] += 1

    def lookup():
        during, install = state["during"], state["install"]
        try:
            fd = os.open(through, os.O_RDONLY | os.O_CLOEXEC)
        except OSError as e:
            if e.errno in TRANSIENT:
                transient("lookup " + TRANSIENT[e.errno], during, install)
            else:
                reject("lookup failed: %s" % e)
            return False
        try:
            body = fd_read(fd)
            try:
                name = fd_path(fd)
            except OSError as e:
                # Removed between the open and now, as an upgrade removes the
                # old version: the kernel has no name for it any more, and
                # its bytes are all there is to judge.
                if os.fstat(fd).st_nlink != 0:
                    reject("current/syndeo was opened, but the kernel cannot name it: %s" % e)
                    return False
                name = None
        except OSError as e:
            reject("current/syndeo was opened, but its descriptor could not be read: %s" % e)
            return False
        finally:
            os.close(fd)
        if name is None:
            found = {by_body.get(body)}
        else:
            found = {by_name.get(name), by_body.get(body)}
        if len(found) != 1 or None in found:
            reject("current/syndeo opened %s, holding %r: not wholly one version" % (name, body[:60]))
            return False
        v = found.pop()
        if not during and (v != state["final"] or name is None):
            reject("a lookup after the last install found %s%s, not %s"
                   % (v, ", removed while open" if name is None else "", state["final"]))
            return False
        count("lookup ok %s%s" % (v, ", removed while open" if name is None else ""))
        return True

    def start():
        during, install = state["during"], state["install"]
        env = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "HOME": "/var/root"}
        try:
            r = subprocess.run([a.command, "--version"], env=env, capture_output=True, text=True, timeout=60)
        except OSError as e:
            if e.errno in TRANSIENT:
                transient("start " + TRANSIENT[e.errno], during, install)
            else:
                reject("start failed: %s" % e)
            return False
        if r.returncode != 0:
            reasons = [m for m in ("No such file or directory", "Invalid argument")
                       if ("%s: %s" % (a.command, m)) in r.stderr]
            if reasons and r.returncode in (2, 126, 127):
                transient("start %s" % ("ENOENT" if reasons[0].startswith("No") else "EINVAL"), during, install)
            else:
                reject("start exited %d: %s" % (r.returncode, r.stderr.strip()[:200]))
            return False
        first = r.stdout.splitlines()[0] if r.stdout else ""
        v = by_output.get(first)
        if v is None:
            reject("start succeeded, saying %r: no version's" % first)
            return False
        if not during and v != state["final"]:
            reject("a start after the last install ran %s, not %s" % (v, state["final"]))
            return False
        count("start ok %s" % v)
        return True

    def loop(fn):
        while state["during"]:
            fn()

    threads = [threading.Thread(target=loop, args=(lookup,)) for _ in range(2)]
    threads += [threading.Thread(target=loop, args=(start,)) for _ in range(3)]
    if os.readlink(current) != a.start:
        ap.error("%s is not current" % a.start)
    for t in threads:
        t.start()
    timings = []
    try:
        time.sleep(1)
        for i, (v, pkg) in enumerate(a.install, 1):
            state["install"] = i
            began = time.monotonic()
            r = subprocess.run([a.installer, "-pkg", pkg, "-target", "/"],
                               capture_output=True, text=True)
            timings.append((v, time.monotonic() - began))
            if a.log:
                with open("%s-%s.installer" % (a.log, v), "w") as f:
                    f.write(r.stdout + r.stderr + "installer exit %d\n" % r.returncode)
            if r.returncode != 0:
                reject("installing %s failed: %s" % (v, (r.stdout + r.stderr).strip()[-300:]))
                break
            if os.readlink(current) != v:
                reject("after installing %s, current is %s" % (v, os.readlink(current)))
                break
            time.sleep(1)
    finally:
        state["during"] = False
        for t in threads:
            t.join()

    state["final"] = os.readlink(current)
    looked = sum(1 for _ in range(a.settle_lookups) if lookup())
    if looked != a.settle_lookups:
        reject("%d of %d lookups after the last install did not succeed" % (a.settle_lookups - looked, a.settle_lookups))
    started = sum(1 for _ in range(a.settle_launches) if start())
    if started != a.settle_launches:
        reject("%d of %d starts after the last install did not succeed" % (a.settle_launches - started, a.settle_launches))

    for v, seconds in timings:
        print("installed %s in %.1f s" % (v, seconds))
    for key in sorted(tally):
        print("%-28s %d" % (key, tally[key]))
    for i in sorted(unavailable):
        first, last, n = unavailable[i]
        print("install %d (%s): %d failed starts or lookups, the first to the last %.0f ms apart (recorded, not judged)"
              % (i, a.install[i - 1][0], n, (last - first) * 1000))
    if not unavailable:
        print("no start or lookup failed during the installs (recorded, not judged)")
    print("current ends at %s" % state["final"])
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
