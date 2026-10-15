"""Submit one file to Apple's notary service, once, and wait for its verdict.

    python3 -I ci/notarize.py binaries-zip TARBALL VERSION APP OUT.zip
    python3 -I ci/notarize.py submit FILE --record DIR
    python3 -I ci/notarize.py wait --record DIR --deadline-minutes N [--interval-seconds S]
    python3 -I ci/notarize.py --self-test

Two steps, so that the submission ID exists before anything waits on it.

`submit` sends FILE with `xcrun notarytool submit --output-format json`, and
no --wait. It writes DIR/submission.json, holding the ID, the file's name and
SHA-256, and the time, as soon as Apple returns the ID. It submits at most
once per record: if DIR already holds a submission, it refuses. A submit that
fails without returning an ID is reported and never retried, because an
upload Apple received but did not acknowledge would otherwise be submitted
twice.

`wait` reads the ID from DIR and asks `notarytool info` for its status,
every S seconds (60 by default), until:
- Accepted: the notary's log is fetched into DIR/log.json too, as Apple
  recommends even for an accepted submission, for its issues and the
  ticket's contents. Best effort: a log that cannot be fetched is recorded in
  DIR/result.json and is not a failure. Exit 0;
- Invalid or Rejected: the notary's log is fetched into DIR/log.json; exit 1;
- the deadline, N minutes after the submission: exit 2. The submission is
  still unresolved, which is not a rejection; it is never resubmitted, and
  its ID stays in DIR so it can be asked about later;
- 10 API errors in a row (a failed, timed-out or unreadable notarytool call):
  exit 3, likewise unresolved.
Every outcome is written to DIR/result.json. Nothing in DIR holds a secret.

`binaries-zip` writes the ZIP Apple is sent for the release's binaries:
exactly the seven executables of TARBALL (syndeo-VERSION-aarch64-apple-
darwin.tar.gz) and APP, Syndeo.app VERSION, signed, as
ci/package-macos-app.sh --check finds it, and nothing else, made with ditto,
as Apple's notary expects. The bundle goes in whole, its signature as it is,
so that the ticket Apple issues for it staples to the same bytes.

The credentials come from MACOS_NOTARY_KEY_BASE64, MACOS_NOTARY_KEY_ID and
MACOS_NOTARY_ISSUER_ID. The key is written to a private temporary file,
mode 0600, and removed whatever happens, interruption included. No value is
printed. xcrun is found on PATH, which is how the self-test puts a stand-in
in its place.
"""
import argparse
import base64
import datetime
import hashlib
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tarfile
import tempfile
import time

NAMES = ["syndeo", "syndeo-net", "syndeo-keystore", "syndeo-agent", "syndeo-proxy", "syndeo-ui", "syndeo-webkit"]
UUID = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
MAX_API_ERRORS = 10
CALL_TIMEOUT = {"submit": 1800, "info": 120, "log": 300}


def say(msg):
    print("notarize: %s" % msg, flush=True)


def now():
    return datetime.datetime.now(datetime.timezone.utc)


def stamp(t):
    return t.isoformat(timespec="seconds").replace("+00:00", "Z")


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def write_json(path, value):
    tmp = path + ".new"
    with open(tmp, "w") as f:
        json.dump(value, f, indent=2, sort_keys=True)
        f.write("\n")
    os.replace(tmp, path)


class Interrupted(Exception):
    pass


def on_signal(signum, frame):
    raise Interrupted(signum)


class Credentials:
    """The notary key as a private file, for the life of a `with` block."""

    def __enter__(self):
        names = ("MACOS_NOTARY_KEY_BASE64", "MACOS_NOTARY_KEY_ID", "MACOS_NOTARY_ISSUER_ID")
        missing = [n for n in names if not os.environ.get(n, "").strip()]
        if missing:
            raise SystemExit("notarize: refusing: not set: %s" % " ".join(missing))
        self.dir = tempfile.mkdtemp(prefix="notary.", dir=os.environ.get("RUNNER_TEMP") or None)
        os.chmod(self.dir, 0o700)
        path = os.path.join(self.dir, "key.p8")
        try:
            data = base64.b64decode(os.environ["MACOS_NOTARY_KEY_BASE64"], validate=False)
        except Exception:
            self.close()
            raise SystemExit("notarize: refusing: MACOS_NOTARY_KEY_BASE64 is not base64")
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "wb") as f:
            f.write(data)
        self.args = ["--key", path, "--key-id", os.environ["MACOS_NOTARY_KEY_ID"],
                     "--issuer", os.environ["MACOS_NOTARY_ISSUER_ID"]]
        return self

    def close(self):
        shutil.rmtree(self.dir, ignore_errors=True)

    def __exit__(self, *exc):
        self.close()
        return False


def notarytool(kind, args, creds):
    """(returncode, parsed JSON or None, stderr tail). Never raises for the tool."""
    cmd = ["xcrun", "notarytool"] + args + creds.args
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=CALL_TIMEOUT[kind])
    except subprocess.TimeoutExpired:
        return None, None, "%s timed out after %ds" % (kind, CALL_TIMEOUT[kind])
    except OSError as e:
        return None, None, "%s could not run: %s" % (kind, e)
    try:
        value = json.loads(r.stdout) if r.stdout.strip() else None
    except ValueError:
        value = None
    return r.returncode, value, r.stderr.strip()[-400:]


def submit(path, record):
    if not os.path.isfile(path):
        raise SystemExit("notarize: no %s" % path)
    os.makedirs(record, exist_ok=True)
    sub = os.path.join(record, "submission.json")
    if os.path.exists(sub):
        raise SystemExit("notarize: refusing: %s already records a submission; it is never resubmitted" % record)
    digest = sha256(path)
    with Credentials() as creds:
        say("submitting %s (sha256 %s), once" % (os.path.basename(path), digest))
        rc, value, err = notarytool("submit", ["submit", path, "--output-format", "json"], creds)
        sid = (value or {}).get("id", "") if isinstance(value, dict) else ""
        if not UUID.match(sid or ""):
            write_json(os.path.join(record, "result.json"),
                       {"outcome": "submit-failed", "file": os.path.basename(path), "sha256": digest,
                        "checked_at": stamp(now()), "message": "no submission ID was returned; nothing was resubmitted"})
            say("the submission failed without an ID (exit %s): %s" % (rc, err or "no output"))
            say("it is not retried: Apple may have received the upload")
            return 1
        write_json(sub, {"id": sid, "file": os.path.basename(path), "sha256": digest, "submitted_at": stamp(now())})
        say("submission ID %s, recorded in %s" % (sid, sub))
        return 0


def fetch_log(sid, record, creds):
    """Fetch the notary's log for SID into RECORD/log.json, and print it.
    True if it was fetched; never submits anything."""
    out = os.path.join(record, "log.json")
    rc, _, err = notarytool("log", ["log", sid, out], creds)
    if rc == 0 and os.path.isfile(out):
        say("the notary's log for %s is in %s:" % (sid, out))
        with open(out) as f:
            for line in f.read().splitlines()[:200]:
                print("    " + line)
        return True
    say("the notary's log for %s could not be fetched: %s" % (sid, err or "no output"))
    return False


def wait(record, deadline_s, interval_s):
    sub = os.path.join(record, "submission.json")
    if not os.path.isfile(sub):
        raise SystemExit("notarize: refusing: %s records no submission to wait for" % record)
    with open(sub) as f:
        s = json.load(f)
    sid = s.get("id", "")
    if not UUID.match(sid):
        raise SystemExit("notarize: refusing: %s holds no valid submission ID" % sub)
    submitted = datetime.datetime.fromisoformat(s["submitted_at"].replace("Z", "+00:00"))
    deadline = submitted + datetime.timedelta(seconds=deadline_s)
    say("waiting for submission %s, submitted %s; deadline %s" % (sid, s["submitted_at"], stamp(deadline)))
    polls = errors = 0
    status = "unknown"
    result = {"id": sid, "file": s.get("file"), "sha256": s.get("sha256"), "submitted_at": s["submitted_at"]}
    with Credentials() as creds:
        while True:
            rc, value, err = notarytool("info", ["info", sid, "--output-format", "json"], creds)
            polls += 1
            if rc == 0 and isinstance(value, dict) and value.get("status"):
                errors = 0
                status = value["status"]
            else:
                errors += 1
                say("info failed (%d in a row): %s" % (errors, err or "no readable output"))
            t = now()
            result.update({"status": status, "polls": polls, "checked_at": stamp(t)})
            if status == "Accepted":
                result.update(outcome="accepted", message="Apple accepted it")
                write_json(os.path.join(record, "result.json"), result)
                say("submission %s: Accepted, after %s" % (sid, str(t - submitted).split(".")[0]))
                # Apple's log, even for an accepted submission. Best effort: the
                # verdict is already recorded, and a log that cannot be had is
                # noted beside it, not a failure.
                result.update(log="log.json" if fetch_log(sid, record, creds) else "not fetched")
                write_json(os.path.join(record, "result.json"), result)
                return 0
            if status in ("Invalid", "Rejected"):
                result.update(outcome=status.lower(), message="Apple did not accept it; see log.json")
                write_json(os.path.join(record, "result.json"), result)
                say("submission %s: %s" % (sid, status))
                fetch_log(sid, record, creds)
                return 1
            if errors >= MAX_API_ERRORS:
                result.update(outcome="unresolved-api-errors",
                              message="%d notarytool calls in a row failed; the submission is unresolved, not rejected, "
                                      "and was not resubmitted" % errors)
                write_json(os.path.join(record, "result.json"), result)
                say("submission %s is unresolved: %d API errors in a row. It was not rejected and is not resubmitted; "
                    "ask about it later with notarytool info %s" % (sid, errors, sid))
                return 3
            if t >= deadline:
                result.update(outcome="unresolved-deadline",
                              message="still %s at the deadline; this is not a rejection, and it was not resubmitted" % status)
                write_json(os.path.join(record, "result.json"), result)
                say("submission %s is still %s at the deadline, %s after it was submitted. That is not a rejection: "
                    "Apple has not decided. It is not resubmitted; ask about it later with notarytool info %s"
                    % (sid, status, str(t - submitted).split(".")[0], sid))
                return 2
            say("%s: %s (poll %d)" % (stamp(t), status, polls))
            time.sleep(max(0.0, min(interval_s, (deadline - now()).total_seconds())))


def bundle_files(state):
    """The files of Syndeo.app, signed or stapled, as the ZIP lists them."""
    here = os.path.dirname(os.path.abspath(__file__))
    listed = subprocess.run(["bash", os.path.join(here, "package-macos-app.sh"), "--manifest", state],
                            capture_output=True, text=True, check=True).stdout.splitlines()
    return ["notarize/Syndeo.app/" + line.split()[0] for line in listed if line.split()[1] == "f"]


def binaries_zip(tarball, version, app, out):
    name = "syndeo-%s-aarch64-apple-darwin" % version
    here = os.path.dirname(os.path.abspath(__file__))
    if os.path.basename(tarball) != name + ".tar.gz":
        raise SystemExit("notarize: %s is not %s.tar.gz" % (tarball, name))
    if os.path.basename(app.rstrip("/")) != "Syndeo.app" or os.path.islink(app) or not os.path.isdir(app):
        raise SystemExit("notarize: %s is not a Syndeo.app" % app)
    if os.path.exists(out):
        raise SystemExit("notarize: %s exists" % out)
    checked = subprocess.run(["bash", os.path.join(here, "package-macos-app.sh"), "--check", app, version, "signed"],
                             capture_output=True, text=True)
    if checked.returncode != 0:
        raise SystemExit("notarize: %s is not exactly Syndeo.app %s, signed:\n%s" % (app, version, checked.stdout + checked.stderr))
    work = tempfile.mkdtemp(prefix="binaries.")
    try:
        stage = os.path.join(work, "notarize")
        os.mkdir(stage)
        with tarfile.open(tarball, "r:gz") as t:
            members = {m.name: m for m in t.getmembers()}
            for n in NAMES:
                m = members.get("%s/%s" % (name, n))
                if m is None or not m.isfile():
                    raise SystemExit("notarize: %s holds no regular file %s" % (tarball, n))
                src = t.extractfile(m)
                dst = os.path.join(stage, n)
                with open(dst, "wb") as f:
                    shutil.copyfileobj(src, f)
                os.chmod(dst, 0o755)
        subprocess.run(["/usr/bin/ditto", app, os.path.join(stage, "Syndeo.app")], check=True)
        subprocess.run(["/usr/bin/ditto", "-c", "-k", "--keepParent", stage, out], check=True)
        listed = subprocess.run(["/usr/bin/zipinfo", "-1", out], capture_output=True, text=True, check=True).stdout.splitlines()
        files = sorted(x for x in listed if not x.endswith("/"))
        want = sorted(["notarize/" + n for n in NAMES] + bundle_files("signed"))
        if files != want:
            os.remove(out)
            raise SystemExit("notarize: the ZIP holds %s, not exactly the seven executables and Syndeo.app" % files)
        say("%s: exactly the seven executables of %s, and Syndeo.app %s" % (out, os.path.basename(tarball), version))
        return 0
    finally:
        shutil.rmtree(work, ignore_errors=True)


# ------------------------------------------------------------------ self-test

STANDIN = r'''#!/usr/bin/env python3
import json, os, sys
calls = os.environ["STANDIN_CALLS"]
args = sys.argv[1:]
kind = args[1] if len(args) > 1 else ""
key = args[args.index("--key") + 1] if "--key" in args else ""
with open(calls, "a") as f:
    f.write("%s %s\n" % (kind, args[2] if len(args) > 2 and kind != "submit" else ""))
with open(os.environ["STANDIN_KEYS"], "a") as f:
    f.write(key + "\n")
if open(key).read() != "notary-key":
    sys.exit(9)
plan = os.environ.get("STANDIN_PLAN", "Accepted").split(",")
state = os.environ["STANDIN_STATE"]
n = int(open(state).read()) if os.path.exists(state) else 0
if kind == "submit":
    if os.environ.get("STANDIN_SUBMIT") == "fail":
        sys.stderr.write("Error: HTTP status code: 500\n"); sys.exit(1)
    print(json.dumps({"id": "11111111-2222-4333-8444-555555555555", "message": "Successfully uploaded file", "path": args[2]}))
elif kind == "info":
    step = plan[min(n, len(plan) - 1)]
    open(state, "w").write(str(n + 1))
    if step == "error":
        sys.stderr.write("Error: the request timed out\n"); sys.exit(1)
    if step == "garbage":
        print("not json"); sys.exit(0)
    if step == "hang":
        import time; time.sleep(5)
    print(json.dumps({"id": args[2], "status": step, "name": "notarize.zip", "createdDate": "2026-10-12T03:36:55.304Z"}))
elif kind == "log":
    # notarytool log ID OUTPUT, then the credential options. The log says
    # what the plan ended in: an accepted one carries no issues and a ticket.
    if os.environ.get("STANDIN_LOG") == "fail":
        sys.stderr.write("Error: HTTP status code: 404\n"); sys.exit(1)
    if plan[-1] == "Accepted":
        log = {"status": "Accepted", "issues": None, "ticketContents": [{"path": "notarize.zip/notarize/syndeo", "digestAlgorithm": "SHA-256", "cdhash": "0123456789abcdef0123456789abcdef01234567", "arch": "arm64"}]}
    else:
        log = {"status": plan[-1], "issues": [{"severity": "error", "path": "notarize.zip/notarize/syndeo", "message": "The binary is not signed with a valid Developer ID certificate.", "architecture": "arm64"}]}
    open(args[3], "w").write(json.dumps(log, indent=2))
else:
    sys.exit(2)
'''


def self_test():
    cases = wrong = 0
    root = tempfile.mkdtemp(prefix="notarize-self-test.")
    bindir = os.path.join(root, "bin")
    os.mkdir(bindir)
    xcrun = os.path.join(bindir, "xcrun")
    with open(xcrun, "w") as f:
        f.write(STANDIN)
    os.chmod(xcrun, 0o755)
    me = os.path.abspath(__file__)

    def run(label, args, want_rc, want_text, plan="Accepted", submit_mode="", prepare=None, check=None, signal_after=None,
            want_calls=None, drop=(), log_mode=""):
        nonlocal cases, wrong
        case = tempfile.mkdtemp(dir=root)
        record = os.path.join(case, "record")
        temp = os.path.join(case, "temp")
        os.makedirs(temp)
        payload = os.path.join(case, "notarize.zip")
        with open(payload, "wb") as f:
            f.write(b"PK\x03\x04 a stand-in")
        env = {"PATH": bindir + ":/usr/bin:/bin", "RUNNER_TEMP": temp,
               "STANDIN_CALLS": os.path.join(case, "calls"), "STANDIN_KEYS": os.path.join(case, "keys"),
               "STANDIN_STATE": os.path.join(case, "state"), "STANDIN_PLAN": plan, "STANDIN_SUBMIT": submit_mode,
               "STANDIN_LOG": log_mode,
               "MACOS_NOTARY_KEY_BASE64": base64.b64encode(b"notary-key").decode(),
               "MACOS_NOTARY_KEY_ID": "SECRETKEYID", "MACOS_NOTARY_ISSUER_ID": "SECRET-ISSUER-VALUE"}
        for name in drop:
            env.pop(name)
        if prepare:
            prepare(record, payload)
        argv = [a.replace("@record", record).replace("@payload", payload) for a in args]
        # Run from inside the case, so nothing can be written anywhere else.
        p = subprocess.Popen([sys.executable, "-I", me] + argv, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                             text=True, cwd=case)
        if signal_after is not None:
            time.sleep(signal_after)
            p.send_signal(signal.SIGTERM)
        out, _ = p.communicate(timeout=120)
        calls = open(env["STANDIN_CALLS"]).read().split("\n") if os.path.exists(env["STANDIN_CALLS"]) else []
        calls = [c.strip() for c in calls if c.strip()]
        problems = []
        if p.returncode != want_rc:
            problems.append("exit %s, not %s" % (p.returncode, want_rc))
        for t in want_text:
            if t not in out:
                problems.append("did not say %r" % t)
        for leak in ("notary-key", "SECRETKEYID", "SECRET-ISSUER-VALUE", base64.b64encode(b"notary-key").decode()):
            if leak in out:
                problems.append("printed a secret")
            for dirpath, _, files in os.walk(record):
                for fn in files:
                    if leak in open(os.path.join(dirpath, fn), errors="replace").read():
                        problems.append("a secret in %s" % fn)
        if os.listdir(temp):
            problems.append("left behind in RUNNER_TEMP: %s" % os.listdir(temp))
        if os.path.exists(env["STANDIN_KEYS"]):
            for k in open(env["STANDIN_KEYS"]).read().split():
                if os.path.exists(k):
                    problems.append("the key file survived")
        if want_calls is not None and calls != want_calls:
            problems.append("calls %s, not %s" % (calls, want_calls))
        if check:
            # A check that cannot even read the record is a wrong case, not a crash.
            try:
                problems += check(record, calls, out) or []
            except Exception as e:
                problems.append("the check failed: %s: %s" % (e.__class__.__name__, e))
        strays = sorted(set(os.listdir(case)) - {"record", "temp", "notarize.zip", "calls", "keys", "state"})
        if strays:
            problems.append("wrote outside the record: %s" % strays)
        cases += 1
        if problems:
            wrong += 1
            print("  WRONG %s: %s" % (label, "; ".join(problems)))
            print("\n".join("          | " + line for line in out.splitlines()[-12:]))
        else:
            print("  ok    %s" % label)

    sid = "11111111-2222-4333-8444-555555555555"

    def submitted(record, payload, when=None):
        os.makedirs(record)
        write_json(os.path.join(record, "submission.json"),
                   {"id": sid, "file": "notarize.zip", "sha256": sha256(payload),
                    "submitted_at": stamp(when or now())})

    def outcome(want):
        def check(record, calls, out):
            r = json.load(open(os.path.join(record, "result.json")))
            return [] if r.get("outcome") == want and r.get("id") in (sid, None) else ["result %s" % r.get("outcome")]
        return check

    print("\n  submitting, once")

    def recorded_before_return(record, calls, out):
        s = json.load(open(os.path.join(record, "submission.json")))
        return [] if s["id"] == sid and UUID.match(s["id"]) and len(s["sha256"]) == 64 else ["submission.json %s" % s]
    run("submit: one call, the ID recorded with the file's SHA-256", ["submit", "@payload", "--record", "@record"], 0,
        ["submission ID %s, recorded" % sid], want_calls=["submit"], check=recorded_before_return)
    run("submit into a record that already holds a submission: refused, nothing sent",
        ["submit", "@payload", "--record", "@record"], 1, ["never resubmitted"], prepare=submitted, want_calls=[])
    run("submit that fails without an ID: reported, not retried", ["submit", "@payload", "--record", "@record"], 1,
        ["failed without an ID", "not retried"], submit_mode="fail", want_calls=["submit"],
        check=lambda r, c, o: [] if not os.path.exists(os.path.join(r, "submission.json")) else ["a submission was recorded"])

    print("\n  waiting, never submitting")
    w = ["wait", "--record", "@record", "--deadline-seconds", "4", "--interval-seconds", "0.2"]
    def accepted_with_log(record, calls, out):
        r = json.load(open(os.path.join(record, "result.json")))
        log = json.load(open(os.path.join(record, "log.json")))
        return outcome("accepted")(record, calls, out) + (
            [] if r.get("log") == "log.json" and log.get("status") == "Accepted" and log.get("ticketContents") else
            ["the accepted submission's log not kept: %s" % r.get("log")])
    run("Accepted at once: exit 0, and Apple's log for it kept", w, 0, ["Accepted", "ticketContents"], prepare=submitted,
        want_calls=["info " + sid, "log " + sid], check=accepted_with_log)
    run("In Progress twice, then Accepted: polled until accepted", w, 0, ["In Progress (poll 1)", "Accepted"],
        plan="In Progress,In Progress,Accepted", prepare=submitted, want_calls=["info " + sid] * 3 + ["log " + sid],
        check=accepted_with_log)
    run("Accepted, but its log cannot be fetched: still exit 0, the miss recorded, nothing resubmitted", w, 0,
        ["Accepted", "could not be fetched"], prepare=submitted, want_calls=["info " + sid, "log " + sid], log_mode="fail",
        check=lambda r, c, o: outcome("accepted")(r, c, o) + (
            [] if json.load(open(os.path.join(r, "result.json"))).get("log") == "not fetched" and
            not os.path.exists(os.path.join(r, "log.json")) else ["the miss is not recorded"]))
    run("Invalid: the log fetched and kept, exit 1", w, 1, ["Invalid", "The binary is not signed"], plan="Invalid",
        prepare=submitted, want_calls=["info " + sid, "log " + sid],
        check=lambda r, c, o: outcome("invalid")(r, c, o) + ([] if os.path.isfile(os.path.join(r, "log.json")) else ["no log.json"]))
    run("Rejected: the log fetched, exit 1", w, 1, ["Rejected", "The binary is not signed"], plan="Rejected", prepare=submitted,
        want_calls=["info " + sid, "log " + sid],
        check=lambda r, c, o: outcome("rejected")(r, c, o) + ([] if os.path.isfile(os.path.join(r, "log.json")) else ["no log.json"]))
    run("two API errors, then Accepted: the errors tolerated", w, 0, ["info failed (1 in a row)", "Accepted"],
        plan="error,garbage,Accepted", prepare=submitted, want_calls=["info " + sid] * 3 + ["log " + sid],
        check=outcome("accepted"))
    run("API errors ten in a row: unresolved, exit 3, not a rejection, nothing resubmitted",
        ["wait", "--record", "@record", "--deadline-seconds", "60", "--interval-seconds", "0.05"], 3,
        ["unresolved", "It was not rejected and is not resubmitted"], plan="error", prepare=submitted,
        want_calls=["info " + sid] * MAX_API_ERRORS, check=outcome("unresolved-api-errors"))

    def no_reject_word(record, calls, out):
        bad = [w for w in ("rejected", "Rejected", "invalid", "Invalid") if w in out.replace("not a rejection", "")]
        return (["the timeout claims %s" % bad] if bad else []) + (["a submit"] if "submit" in calls else [])
    run("the deadline: unresolved, exit 2, not a rejection, ID kept, nothing resubmitted",
        ["wait", "--record", "@record", "--deadline-seconds", "1", "--interval-seconds", "0.3"], 2,
        ["still In Progress at the deadline", "That is not a rejection", "notarytool info " + sid],
        plan="In Progress", prepare=submitted,
        check=lambda r, c, o: outcome("unresolved-deadline")(r, c, o) + no_reject_word(r, c, o) +
        ([] if json.load(open(os.path.join(r, "submission.json")))["id"] == sid else ["the ID was lost"]))
    run("a deadline already past at the first poll: one poll, then unresolved",
        ["wait", "--record", "@record", "--deadline-seconds", "1", "--interval-seconds", "0.3"], 2,
        ["That is not a rejection"], plan="In Progress",
        prepare=lambda r, p: submitted(r, p, now() - datetime.timedelta(minutes=5)), want_calls=["info " + sid])
    run("wait with no submission recorded: refused, no call", w, 1, ["records no submission"], want_calls=[])
    run("wait with a malformed ID: refused, no call", w, 1, ["no valid submission ID"],
        prepare=lambda r, p: (os.makedirs(r), write_json(os.path.join(r, "submission.json"),
                                                         {"id": "abc", "submitted_at": stamp(now())})), want_calls=[])
    run("interrupted while waiting: exit 143, the key file removed, the ID kept",
        ["wait", "--record", "@record", "--deadline-seconds", "60", "--interval-seconds", "0.2"], 143,
        [], plan="In Progress", prepare=submitted, signal_after=1.0,
        check=lambda r, c, o: [] if json.load(open(os.path.join(r, "submission.json")))["id"] == sid else ["the ID was lost"])

    print("\n  credentials")
    run("no notary key set: refused before any call", ["submit", "@payload", "--record", "@record"], 1,
        ["not set: MACOS_NOTARY_KEY_BASE64"], want_calls=[], drop=("MACOS_NOTARY_KEY_BASE64",))
    run("no issuer set: refused before any call", w, 1, ["not set: MACOS_NOTARY_ISSUER_ID"], prepare=submitted,
        want_calls=[], drop=("MACOS_NOTARY_ISSUER_ID",))

    print("\n  the binaries' ZIP")
    if sys.platform != "darwin":
        print("  (macOS only: ditto and zipinfo; not run on %s)" % sys.platform)
    else:
        def tarball(names, version="0.9.1"):
            d = tempfile.mkdtemp(dir=root)
            top = os.path.join(d, "src", "syndeo-%s-aarch64-apple-darwin" % version)
            os.makedirs(os.path.join(top, "tools"))
            for n in names + ["README.md", "LICENSE", "tools/wordcount.wat"]:
                with open(os.path.join(top, n), "w") as f:
                    f.write(n)
            path = os.path.join(d, "syndeo-%s-aarch64-apple-darwin.tar.gz" % version)
            subprocess.run(["tar", "-C", os.path.join(d, "src"), "-czf", path, os.path.basename(top)], check=True)
            return path

        here = os.path.dirname(os.path.abspath(__file__))

        def bundle(version="0.9.1", extra=None):
            """Syndeo.app VERSION as ci/package-macos-app.sh assembles it, with
            its signature's two paths: what --check calls signed."""
            d = tempfile.mkdtemp(dir=root)
            os.mkdir(os.path.join(d, "bins"))
            for n in ("syndeo-app", "syndeo-proxy"):
                with open(os.path.join(d, "bins", n), "w") as f:
                    f.write("#!/bin/sh\n")
            subprocess.run(["bash", os.path.join(here, "package-macos-app.sh"), version, os.path.join(d, "bins"), d],
                           check=True, capture_output=True)
            app = os.path.join(d, "Syndeo.app")
            os.mkdir(os.path.join(app, "Contents", "_CodeSignature"), 0o755)
            with open(os.path.join(app, "Contents", "_CodeSignature", "CodeResources"), "w") as f:
                f.write("seal")
            os.chmod(os.path.join(app, "Contents", "_CodeSignature", "CodeResources"), 0o644)
            if extra:
                with open(os.path.join(app, "Contents", "Resources", extra), "w") as f:
                    f.write("extra")
            return app

        good_app = bundle()

        def zip_case(label, path, want_rc, want_text, want_files=None, app=None):
            nonlocal cases, wrong
            out = os.path.join(tempfile.mkdtemp(dir=root), "notarize.zip")
            r = subprocess.run([sys.executable, "-I", me, "binaries-zip", path, "0.9.1", app or good_app, out],
                               capture_output=True, text=True)
            problems = []
            if r.returncode != want_rc:
                problems.append("exit %s" % r.returncode)
            if want_text not in r.stdout + r.stderr:
                problems.append("did not say %r" % want_text)
            if want_files is not None:
                got = sorted(x for x in subprocess.run(["/usr/bin/zipinfo", "-1", out], capture_output=True, text=True).stdout.split()
                             if not x.endswith("/"))
                if got != want_files:
                    problems.append("holds %s" % got)
            elif os.path.exists(out):
                problems.append("a ZIP was left behind")
            cases += 1
            if problems:
                wrong += 1
                print("  WRONG %s: %s" % (label, "; ".join(problems)))
            else:
                print("  ok    %s" % label)
        zip_case("exactly the seven executables, nothing else from the tarball, and Syndeo.app whole", tarball(list(NAMES)), 0,
                 "exactly the seven executables", sorted(["notarize/" + n for n in NAMES] + bundle_files("signed")))
        zip_case("a tarball missing syndeo-webkit: refused, no ZIP", tarball(NAMES[:-1]), 1, "no regular file syndeo-webkit")
        zip_case("a tarball of another version: refused, no ZIP", tarball(list(NAMES), "0.9.2"), 1, "is not syndeo-0.9.1")
        zip_case("Syndeo.app of another version: refused, no ZIP", tarball(list(NAMES)), 1, "is not exactly Syndeo.app 0.9.1",
                 app=bundle("0.9.2"))
        zip_case("Syndeo.app with a file more: refused, no ZIP", tarball(list(NAMES)), 1, "is not exactly Syndeo.app 0.9.1",
                 app=bundle(extra="extra"))
        unsigned = bundle()
        shutil.rmtree(os.path.join(unsigned, "Contents", "_CodeSignature"))
        zip_case("Syndeo.app unsigned: refused, no ZIP", tarball(list(NAMES)), 1, "is not exactly Syndeo.app 0.9.1", app=unsigned)

    shutil.rmtree(root, ignore_errors=True)
    print("\n  %d cases, %d wrong\n" % (cases, wrong))
    return wrong == 0


def main():
    if sys.argv[1:] == ["--self-test"]:
        return 0 if self_test() else 1
    for s in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(s, on_signal)
    ap = argparse.ArgumentParser(prog="notarize.py")
    sub = ap.add_subparsers(dest="cmd", required=True)
    z = sub.add_parser("binaries-zip")
    z.add_argument("tarball")
    z.add_argument("version")
    z.add_argument("app")
    z.add_argument("out")
    s = sub.add_parser("submit")
    s.add_argument("file")
    s.add_argument("--record", required=True)
    w = sub.add_parser("wait")
    w.add_argument("--record", required=True)
    g = w.add_mutually_exclusive_group(required=True)
    g.add_argument("--deadline-minutes", type=float)
    g.add_argument("--deadline-seconds", type=float)
    w.add_argument("--interval-seconds", type=float, default=60.0)
    a = ap.parse_args()
    try:
        if a.cmd == "binaries-zip":
            return binaries_zip(a.tarball, a.version, a.app, a.out)
        if a.cmd == "submit":
            return submit(a.file, a.record)
        deadline = a.deadline_seconds if a.deadline_seconds is not None else a.deadline_minutes * 60
        return wait(a.record, deadline, a.interval_seconds)
    except Interrupted as e:
        say("interrupted by signal %d; nothing more is sent, and any recorded submission ID is kept" % e.args[0])
        return 128 + e.args[0]


if __name__ == "__main__":
    sys.exit(main())
