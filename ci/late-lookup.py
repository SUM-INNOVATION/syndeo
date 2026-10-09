"""Hold a running `syndeo doctor` across an upgrade, then let it look for its
siblings.

    python3 -I ci/late-lookup.py --command PATH --user USER --home DIR
        --private DIR --old VERSION --new VERSION --held FILE --go FILE

Run as root. It starts COMMAND doctor as USER, with its stdout on a pipe that
is already full, so that the doctor stops at its first line of output: after
`main` has found the directory of the image it runs, and before it has
looked for any sibling. Once the doctor's log says it is past that point, it
writes FILE --held and waits for FILE --go; meanwhile the caller upgrades
from OLD to NEW. Then it empties the pipe, and the doctor goes on to look for
syndeo-net, syndeo-keystore and syndeo-agent, late, from a version that is
gone.

Checked as the package promises: every one of those lookups is refused, with
"this version was removed during an upgrade", and the doctor starts no
program at all, from NEW or anywhere else. Exits 1 otherwise.
"""
import argparse
import fcntl
import os
import pwd
import subprocess
import sys
import time

SIBLINGS = ("syndeo-net", "syndeo-keystore", "syndeo-agent")


def children(pid):
    r = subprocess.run(["/usr/bin/pgrep", "-P", str(pid)], capture_output=True, text=True)
    found = {}
    for c in r.stdout.split():
        args = subprocess.run(["/bin/ps", "-o", "args=", "-p", c], capture_output=True, text=True).stdout.strip()
        found[int(c)] = args
    return found


def main():
    ap = argparse.ArgumentParser()
    for name in ("command", "user", "home", "private", "old", "new", "held", "go"):
        ap.add_argument("--" + name, required=True)
    ap.add_argument("--wait-seconds", type=float, default=600)
    a = ap.parse_args()
    user = pwd.getpwnam(a.user)
    os.makedirs(a.home, exist_ok=True)
    if os.geteuid() == 0:
        os.chown(a.home, user.pw_uid, user.pw_gid)
    problems = []

    # A pipe that is full before the doctor writes to it. One write larger
    # than the kernel's small pipe size makes it the large one first.
    read_end, write_end = os.pipe()
    fcntl.fcntl(write_end, fcntl.F_SETFL, fcntl.fcntl(write_end, fcntl.F_GETFL) | os.O_NONBLOCK)
    filled = 0
    for size in (65536, 4096, 512, 1):
        while True:
            try:
                filled += os.write(write_end, b"#" * size)
            except BlockingIOError:
                break
    fcntl.fcntl(write_end, fcntl.F_SETFL, fcntl.fcntl(write_end, fcntl.F_GETFL) & ~os.O_NONBLOCK)

    log = open(os.path.join(a.home, "doctor.stderr"), "w+")
    env = {
        "PATH": "/usr/local/bin:/usr/bin:/bin",
        "HOME": user.pw_dir,
        "SYNDEO_HOME": a.home,
        # The line that says main is past finding its directory.
        "SYNDEO_LOG": "syndeo=debug",
    }
    # As USER; a trial run that is not root runs it as itself.
    as_user = dict(user=user.pw_uid, group=user.pw_gid, extra_groups=[]) if os.geteuid() == 0 else {}
    doctor = subprocess.Popen([a.command, "doctor"], stdin=subprocess.DEVNULL, stdout=write_end,
                              stderr=log, env=env, **as_user)
    os.close(write_end)
    print("doctor started: pid %d, from %s, its stdout held behind %d bytes" % (doctor.pid, a.command, filled))

    deadline = time.monotonic() + 60
    while True:
        log.seek(0)
        if "example tools" in log.read():
            break
        if doctor.poll() is not None:
            problems.append("the doctor exited (%d) before it was held" % doctor.returncode)
            break
        if time.monotonic() > deadline:
            problems.append("the doctor never logged that it was past finding its directory")
            break
        time.sleep(0.05)
    seen = {}
    if not problems:
        time.sleep(0.5)
        if doctor.poll() is not None:
            problems.append("the doctor was not held: it exited (%d)" % doctor.returncode)
        seen.update(children(doctor.pid))
        if seen:
            problems.append("the doctor started programs before it was held: %s" % seen)
    if problems:
        doctor.kill()
        doctor.wait()
        for p in problems:
            print("PROBLEM: " + p)
        return 1
    print("held: the doctor is past finding its directory, %s, and waits to write its first line"
          % os.path.join(a.private, a.old))
    with open(a.held, "w") as f:
        f.write("%d\n" % doctor.pid)

    deadline = time.monotonic() + a.wait_seconds
    while not os.path.exists(a.go):
        if time.monotonic() > deadline:
            doctor.kill()
            doctor.wait()
            print("PROBLEM: no go within %d seconds" % a.wait_seconds)
            return 1
        if doctor.poll() is not None:
            print("PROBLEM: the doctor exited (%d) while held" % doctor.returncode)
            return 1
        time.sleep(0.1)
    old_present = os.path.lexists(os.path.join(a.private, a.old))
    new_present = os.path.isdir(os.path.join(a.private, a.new))
    current = os.readlink(os.path.join(a.private, "current"))
    print("upgraded: %s %s, %s %s, current -> %s" % (
        a.old, "still there" if old_present else "gone", a.new, "there" if new_present else "MISSING", current))
    if old_present or not new_present or current != a.new:
        problems.append("the upgrade did not leave %s alone and current" % a.new)

    # Let it go, and watch for anything it starts while it runs.
    fcntl.fcntl(read_end, fcntl.F_SETFL, fcntl.fcntl(read_end, fcntl.F_GETFL) | os.O_NONBLOCK)
    output = b""
    deadline = time.monotonic() + 60
    while True:
        seen.update(children(doctor.pid))
        try:
            chunk = os.read(read_end, 65536)
        except BlockingIOError:
            chunk = None
        if chunk == b"":
            # Only the doctor held the other end: it has exited.
            break
        if chunk:
            output += chunk
        else:
            time.sleep(0.01)
        if time.monotonic() > deadline:
            doctor.kill()
            problems.append("the doctor did not finish within 60 seconds")
            break
    status = doctor.wait()
    os.close(read_end)
    text = output[filled:].decode(errors="replace")
    log.seek(0)
    errors = log.read()
    print("the doctor exited %d and said:" % status)
    for line in text.splitlines():
        print("  | " + line)

    for name in SIBLINGS:
        line = next((l for l in text.splitlines() if l.split()[:1] == [name]), "")
        if "NOT FOUND" not in line:
            problems.append("%s was not refused: %r" % (name, line))
    if "this version was removed during an upgrade" not in text:
        problems.append("the doctor did not say the version was removed during an upgrade")
    if any(os.path.join(a.private, a.new) in args for args in seen.values()):
        problems.append("the doctor started a program from %s: %s" % (a.new, seen))
    if seen:
        problems.append("the doctor started programs: %s" % seen)
    if "cannot tell where this program is installed" in errors:
        problems.append("the doctor had not found its directory before the upgrade")
    for p in problems:
        print("PROBLEM: " + p)
    if problems:
        print("stderr:")
        for line in errors.splitlines()[-20:]:
            print("  | " + line)
        return 1
    print("every late lookup was refused, and the doctor started nothing")
    return 0


if __name__ == "__main__":
    sys.exit(main())
