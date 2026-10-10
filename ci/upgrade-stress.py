"""Start Syndeo's commands over and over while Installer upgrades it.

    python3 -I ci/upgrade-stress.py --private DIR --command PATH --from VERSION
        --install VERSION PKG [--install VERSION PKG]...
        --expect-output TEMPLATE --expect-file TEMPLATE
        [--log PREFIX] [--settle-lookups N] [--settle-launches N]
        [--installer PATH]

Run as root, on a disposable runner, with VERSION installed. Each --install
is an upgrade, `installer -dumplog -pkg PKG -target /`, run in turn, with all it
prints kept by --log. What the package
promises of an upgrade, checked as stated:
- while an install runs, a lookup through `current`, or a command started
  through it, either fails with ENOENT or EINVAL, because the commands are
  unavailable while the old version is replaced, or succeeds wholly as that
  install's old or new version: never a mixture, never anything else;
- every install succeeds and leaves `current` at its version;
- once an install has returned, and between installs, and after the last,
  every lookup and every start succeeds, as the version just installed.

A lookup opens DIR/current/syndeo once and judges what that descriptor holds,
not where the file ends up: Installer may move the old version's files into
its trash while one is open. The descriptor has to be a regular file whose
bytes are exactly one expected executable (--expect-file, formatted with the
version), and a file, known by device, inode and birth time, has to hold the
same version every time it is opened. A start runs COMMAND --version; its
first line has to be --expect-output for an expected version, and it counts
as unavailable only when exec, or env opening COMMAND, said "No such file or
directory" or "Invalid argument" for COMMAND itself.

How long the commands were unavailable, from the first such failure to the
last, is printed for each install, and not judged. Exits 1 on any rejection.
--installer replaces /usr/sbin/installer, for trying the judging out without
installing anything.
"""
import argparse
import errno
import os
import stat
import subprocess
import sys
import threading
import time

TRANSIENT = {errno.ENOENT: "ENOENT", errno.EINVAL: "EINVAL"}


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
    by_body = {a.expect_file.format(version=v).encode(): v for v in versions}
    by_output = {a.expect_output.format(version=v): v for v in versions}
    if len(by_body) != len(versions) or len(by_output) != len(versions):
        ap.error("the templates have to tell every version apart")

    # (installing, i): install i, from versions[i - 1] to versions[i], is
    # running. (settled, i): versions[i] is installed and nothing is running.
    # Every lookup and start is judged by the phase it began in.
    state = {"phase": ("settled", 0), "running": True}
    lock = threading.Lock()
    tally = {}
    rejects = []
    unavailable = {}  # install number -> [first, last, count]
    identities = {}  # (device, inode, birth time) -> version

    def allowed(phase):
        kind, i = phase
        return {versions[i - 1], versions[i]} if kind == "installing" else {versions[i]}

    def describe(phase):
        kind, i = phase
        if kind == "installing":
            return "while installing %s over %s" % (versions[i], versions[i - 1])
        return "with %s installed and nothing running" % versions[i]

    def count(key):
        with lock:
            tally[key] = tally.get(key, 0) + 1

    def reject(what):
        with lock:
            rejects.append(what)

    def unavailable_now(name, phase):
        kind, i = phase
        if kind != "installing":
            reject("%s %s" % (name, describe(phase)))
            return
        now = time.monotonic()
        with lock:
            tally["unavailable " + name] = tally.get("unavailable " + name, 0) + 1
            span = unavailable.setdefault(i, [now, now, 0])
            span[1] = now
            span[2] += 1

    def lookup():
        phase = state["phase"]
        try:
            fd = os.open(through, os.O_RDONLY | os.O_CLOEXEC)
        except OSError as e:
            if e.errno in TRANSIENT:
                unavailable_now("lookup " + TRANSIENT[e.errno], phase)
            else:
                reject("a lookup failed %s: %s" % (describe(phase), e))
            return False
        # From here on, only what this descriptor holds.
        try:
            st = os.fstat(fd)
            body = fd_read(fd)
        except OSError as e:
            reject("current/syndeo was opened, but its descriptor could not be read: %s" % e)
            return False
        finally:
            os.close(fd)
        if not stat.S_ISREG(st.st_mode):
            reject("current/syndeo opened something that is not a regular file (mode %o)" % st.st_mode)
            return False
        v = by_body.get(body)
        if v is None or v not in allowed(phase):
            reject("current/syndeo held %r %s: not %s"
                   % (body[:60], describe(phase), " or ".join(sorted(allowed(phase)))))
            return False
        identity = (st.st_dev, st.st_ino, getattr(st, "st_birthtime", 0))
        with lock:
            known = identities.setdefault(identity, v)
        if known != v:
            reject("one file, inode %d, held %s and later %s" % (st.st_ino, known, v))
            return False
        count("lookup ok %s" % v)
        return True

    def start():
        phase = state["phase"]
        env = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "HOME": "/var/root"}
        try:
            r = subprocess.run([a.command, "--version"], env=env, capture_output=True, text=True, timeout=60)
        except OSError as e:
            if e.errno in TRANSIENT:
                unavailable_now("start " + TRANSIENT[e.errno], phase)
            else:
                reject("a start failed %s: %s" % (describe(phase), e))
            return False
        if r.returncode != 0:
            reasons = [m for m in ("No such file or directory", "Invalid argument")
                       if ("%s: %s" % (a.command, m)) in r.stderr]
            if reasons and r.returncode in (2, 126, 127):
                unavailable_now("start %s" % ("ENOENT" if reasons[0].startswith("No") else "EINVAL"), phase)
            else:
                reject("a start exited %d %s: %s" % (r.returncode, describe(phase), r.stderr.strip()[:200]))
            return False
        first = r.stdout.splitlines()[0] if r.stdout else ""
        v = by_output.get(first)
        if v is None or v not in allowed(phase):
            reject("a start said %r %s" % (first, describe(phase)))
            return False
        count("start ok %s" % v)
        return True

    def loop(fn):
        while state["running"]:
            fn()

    if os.readlink(current) != a.start:
        ap.error("%s is not current" % a.start)
    threads = [threading.Thread(target=loop, args=(lookup,)) for _ in range(2)]
    threads += [threading.Thread(target=loop, args=(start,)) for _ in range(3)]
    for t in threads:
        t.start()
    timings = []
    try:
        time.sleep(1)
        for i, (v, pkg) in enumerate(a.install, 1):
            state["phase"] = ("installing", i)
            began = time.monotonic()
            r = subprocess.run([a.installer, "-dumplog", "-pkg", pkg, "-target", "/"],
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
            state["phase"] = ("settled", i)
            time.sleep(1)
    finally:
        state["running"] = False
        for t in threads:
            t.join()

    final = os.readlink(current)
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
    print("files seen, by device, inode and birth time: %d" % len(identities))
    for i in sorted(unavailable):
        first, last, n = unavailable[i]
        print("install %d (%s): %d failed starts or lookups, the first to the last %.0f ms apart (recorded, not judged)"
              % (i, a.install[i - 1][0], n, (last - first) * 1000))
    if not unavailable:
        print("no start or lookup failed during the installs (recorded, not judged)")
    print("current ends at %s" % final)
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
