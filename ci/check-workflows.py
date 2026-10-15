"""Check where the workflows let signing secrets go.

    python3 -I ci/check-workflows.py [WORKFLOW-DIR]
    python3 -I ci/check-workflows.py --self-test

ci.yml runs on every pull request, so it may reference no `secrets.` and no
`environment:` at all, and may not run on pull_request_target, which would
hand a fork's code the repository's secrets.

release.yml holds the signing and notarization secrets. Four jobs may see
them, each only its own share, and each declares the release-macos
environment; the environment scopes the signing configuration to them and
is not an approval gate. Checked:
- it runs on tags and workflow_dispatch, never on a pull request or another
  workflow's run;
- nothing outside the jobs reads a secret or the signing configuration;
- each `secrets.` name is read only by a job approved for it:
  signing-config (all ten, and `vars.SYNDEO_EXPECT_SIGNED`, which no other
  job reads), build-macos (the Application certificate, its password, its
  identity, the team), notarize-binaries (the notary key, its id, its issuer,
  the team) and package (the Installer certificate, its password, its
  identity, the team, and the notary key, id and issuer);
- exactly those four declare `environment: release-macos`, and no job
  declares another;
- none of them can write to the repository: the token everything runs with
  is read-only at the top, and they do not raise it;
- in each, ci/check-signing-config.sh runs first, for its own stage, before
  anything signs or notarizes; signing-config signs nothing;
- the macOS release runs in order: build-macos, then notarize-binaries, then
  package, then pkg-install and checksums, with exactly the needs that make
  it so, and no job-level `if:` but publish's and rehearse's;
- the tarball's and Syndeo.app's provenance: build-macos uploads each, the
  bundle as a ZIP, and gives their SHA-256s; notarize-binaries requires those
  bytes, and puts that bundle in its one submission with the seven
  executables; package requires the bytes notarize-binaries checked, requires
  their notarization accepted, staples that bundle (if signed) or unpacks it,
  and builds the package from it only afterwards;
- every signed binary and the signed bundle checked and run before Apple
  sees any of them: in build-macos once signed, and in notarize-binaries
  before the ZIP and the submission;
- Apple is asked once per file: one `notarize.py submit`, then one `wait`,
  in notarize-binaries and in package and nowhere else, and notarytool is
  never called directly; package signs before it submits and staples
  (`sign-macos-pkg.sh finish`) only after the wait, then inspects;
- each notarizing job's wait ends inside its own time: timeout-minutes at
  most 355, the deadline at most 300 (360 less 60 kept for setup,
  diagnostics and cleanup), and at least 30 minutes between them;
- only publish and rehearse raise the token to `contents: write`, and
  nothing asks for write-all;
- publish and rehearse each need exactly verify, build-macos, pkg-install
  and checksums;
- every `gh release create` makes a draft (`--draft`), and publish's names
  its tag with `--verify-tag`;
- only publish makes a release public (`--draft=false`), only for a tag,
  and only after `verify-pkg.sh release check` has checked its draft;
- rehearse publishes nothing, and its `if: always()` step deletes its draft
  and then fails if either the draft or a tag of its name remains.

The workflows are read line by line, as they are written here: two-space
indentation, jobs as `  name:` under `jobs:`. Anything it cannot place is a
failure, not a pass. Exits 1 on any finding.
"""
import os
import re
import sys

SECRETS = re.compile(r"secrets\.")
ENV_KEY = re.compile(r"^\s*environment:")
JOB = re.compile(r"^  ([A-Za-z0-9_-]+):\s*$")
TOP = re.compile(r"^[A-Za-z0-9_-]+:")
SIGNS = re.compile(r"sign-macos(-pkg)?\.sh|\bproductsign\b|\bcodesign\b|notarytool|notarize\.py (submit|wait)")
SECRET_NAME = re.compile(r"secrets\.([A-Za-z0-9_]+)")
APPLICATION = {"MACOS_CERTIFICATE_P12_BASE64", "MACOS_CERTIFICATE_PASSWORD", "MACOS_SIGNING_IDENTITY"}
INSTALLER = {"MACOS_INSTALLER_CERTIFICATE_P12_BASE64", "MACOS_INSTALLER_CERTIFICATE_PASSWORD", "MACOS_INSTALLER_SIGNING_IDENTITY"}
NOTARY = {"MACOS_NOTARY_KEY_BASE64", "MACOS_NOTARY_KEY_ID", "MACOS_NOTARY_ISSUER_ID"}
TEAM = {"MACOS_TEAM_ID"}
# job: (the secrets it may read, the stage its gate checks; None: the whole gate)
SIGNING_JOBS = {
    "signing-config": (APPLICATION | INSTALLER | NOTARY | TEAM, None),
    "build-macos": (APPLICATION | TEAM, "binary-sign"),
    "notarize-binaries": (NOTARY | TEAM, "binary-notarize"),
    "package": (INSTALLER | NOTARY | TEAM, "package"),
}
ORDER = ["verify", "signing-config", "build-macos", "notarize-binaries", "package", "pkg-install", "checksums"]
GRAPH = {
    "signing-config": ["verify"],
    "build-macos": ["signing-config", "verify"],
    "notarize-binaries": ["build-macos", "signing-config", "verify"],
    "package": ["build-macos", "notarize-binaries", "signing-config", "verify"],
    "pkg-install": ["build-macos", "notarize-binaries", "package", "verify"],
    "checksums": ["build-linux", "build-macos", "notarize-binaries", "package", "verify"],
}
JOB_IFS = {"publish": "if: needs.verify.outputs.publish == 'true'", "rehearse": "if: needs.verify.outputs.publish != 'true'"}


def code(line):
    """The line without a trailing comment, and blank if it is one."""
    s = line.split(" #", 1)[0]
    return "" if s.lstrip().startswith("#") else s


def check_ci(text):
    found = []
    for n, line in enumerate(text.splitlines(), 1):
        c = code(line)
        if SECRETS.search(c):
            found.append("ci.yml:%d references a secret" % n)
        if ENV_KEY.match(c):
            found.append("ci.yml:%d declares an environment" % n)
        if "pull_request_target" in c:
            found.append("ci.yml:%d runs on pull_request_target" % n)
    return found


def blocks(text):
    """(top-level lines, {job: [(line number, code)]}, job order)."""
    top, jobs, order = [], {}, []
    section, job = None, None
    for n, line in enumerate(text.splitlines(), 1):
        c = code(line)
        if TOP.match(line):
            section = line.split(":", 1)[0]
            job = None
            top.append((n, c))
            continue
        if section == "jobs":
            m = JOB.match(line)
            if m:
                job = m.group(1)
                jobs[job] = []
                order.append(job)
                continue
            if job is None:
                if c.strip():
                    top.append((n, c))
                continue
            jobs[job].append((n, c))
        else:
            top.append((n, c))
    return top, jobs, order


def joined(lines):
    """A job's commands, continuation lines joined: [(first line, text)]."""
    out, buf, start = [], [], None
    for n, c in lines:
        if not buf:
            start = n
        stripped = c.rstrip()
        if stripped.endswith("\\"):
            buf.append(stripped[:-1].strip())
            continue
        buf.append(c.strip())
        out.append((start, " ".join(x for x in buf if x)))
        buf = []
    if buf:
        out.append((start, " ".join(x for x in buf if x)))
    return out


def steps(lines):
    """A job's steps, each the [(line, code)] from its `      - ` on."""
    result, cur, inside = [], None, False
    for n, c in lines:
        if c.startswith("    steps:"):
            inside = True
            continue
        if not inside:
            continue
        if re.match(r"^    \S", c):
            inside = False
            continue
        if c.startswith("      - "):
            cur = [(n, c)]
            result.append(cur)
        elif cur is not None:
            cur.append((n, c))
    return result


def needs_of(lines):
    """The jobs a job needs, sorted; None if written neither as [a, b] nor as one name."""
    for n, c in lines:
        m = re.match(r"^    needs:\s*(.*)$", c)
        if m:
            v = m.group(1).strip()
            if v.startswith("[") and v.endswith("]"):
                return sorted(x.strip() for x in v[1:-1].split(",") if x.strip())
            if re.match(r"^[A-Za-z0-9_-]+$", v):
                return [v]
            return None
    return []


DRAFT = re.compile(r"(?<!\S)--draft(?!\S)")
PUBLISHES = re.compile(r"--draft[= ]false|draft=false|\"draft\":\s*false|gh release edit")
RELEASE_NEEDS = sorted(ORDER)


def check_publication(jobs):
    """The draft-first publication, and the dry run's rehearsal of it."""
    found = []
    for name, lines in jobs.items():
        if any(c.strip() == "contents: write" for _, c in lines) and name not in ("publish", "rehearse"):
            found.append("release.yml: %s raises the token to contents: write; only publish and rehearse may" % name)
        if any("write-all" in c for _, c in lines):
            found.append("release.yml: %s asks for write-all" % name)
        for n, cmd in joined(lines):
            if "gh release create" in cmd and not DRAFT.search(cmd):
                found.append("release.yml:%d %s creates a release that is not a draft" % (n, name))
            if PUBLISHES.search(cmd) and name != "publish":
                found.append("release.yml:%d %s publishes a release; only publish may" % (n, name))
    for name in ("publish", "rehearse"):
        if name not in jobs:
            found.append("release.yml: no %s job" % name)
            continue
        needs = needs_of(jobs[name])
        if needs != RELEASE_NEEDS:
            found.append("release.yml: %s needs %s, not exactly %s"
                         % (name, "something it does not write as [a, b]" if needs is None else (needs or "nothing"), ", ".join(ORDER)))
    if "publish" in jobs:
        cmds = joined(jobs["publish"])
        creates = [c for _, c in cmds if "gh release create" in c]
        if not creates or any("--verify-tag" not in c for c in creates):
            found.append("release.yml: publish creates its draft without --verify-tag")
        checks = [n for n, c in cmds if "verify-pkg.sh release check" in c]
        public = [n for n, c in cmds if "--draft=false" in c]
        if not checks:
            found.append("release.yml: publish never checks its draft with verify-pkg.sh release check")
        elif any(n < min(checks) for n in public):
            found.append("release.yml:%d publish makes the release public before checking its draft" % min(public))
    if "rehearse" in jobs:
        cleanup = [st for st in steps(jobs["rehearse"])
                   if any(c.strip() == "if: always()" for _, c in st)
                   and any('gh release delete "$TAG"' in c for _, c in st)]
        if not cleanup:
            found.append("release.yml: rehearse has no if: always() step that deletes its draft")
        else:
            cmds = [c for _, c in joined(cleanup[0])]

            def after(i, needle):
                for j in range(i + 1, len(cmds)):
                    if needle in cmds[j]:
                        return j
                return None
            d = after(-1, 'gh release delete "$TAG"')
            v = after(d, 'gh release view "$TAG"')
            e1 = after(v, "exit 1") if v is not None else None
            t = after(e1, "git/ref/tags/$TAG") if e1 is not None else None
            e2 = after(t, "exit 1") if t is not None else None
            if None in (v, e1):
                found.append("release.yml: rehearse's cleanup does not fail when its draft remains after deleting it")
            if None in (t, e2):
                found.append("release.yml: rehearse's cleanup does not fail when a tag of its draft's name exists")
    return found


def check_release(text):
    found = []
    top, jobs, _ = blocks(text)
    if not jobs:
        return ["release.yml: no jobs found"]
    on = []
    in_on = False
    for n, c in top:
        if re.match(r"^on:", c):
            in_on = True
            continue
        if TOP.match(c):
            in_on = False
        if in_on:
            on.append((n, c))
    for n, c in on:
        if re.search(r"\b(pull_request|pull_request_target|workflow_run)\b", c):
            found.append("release.yml:%d runs on %s" % (n, c.strip().rstrip(":")))
    for n, c in top:
        if SECRETS.search(c) or "vars.SYNDEO_EXPECT_SIGNED" in c:
            found.append("release.yml:%d reads a secret or the signing configuration outside any job" % n)
    perms = [c.strip() for n, c in top]
    if "permissions:" not in perms or "contents: read" not in perms or "contents: write" in perms:
        found.append("release.yml: the top-level token is not read-only (permissions: contents: read)")
    for name, lines in jobs.items():
        envs = [(n, c.strip()) for n, c in lines if ENV_KEY.match(c)]
        scoped = any(c == "environment: release-macos" for _, c in envs)
        allowed = SIGNING_JOBS.get(name, (set(), None))[0]
        for n, c in envs:
            if c != "environment: release-macos" or name not in SIGNING_JOBS:
                found.append("release.yml:%d %s declares %r; only %s may, as release-macos" % (n, name, c, ", ".join(SIGNING_JOBS)))
        if name in SIGNING_JOBS and not scoped:
            found.append("release.yml: %s must declare environment: release-macos" % name)
        for n, c in lines:
            for secret in SECRET_NAME.findall(c):
                if secret not in allowed:
                    found.append("release.yml:%d %s reads secrets.%s, which it is not approved for" % (n, name, secret))
            if SECRETS.search(c) and not SECRET_NAME.search(c):
                found.append("release.yml:%d %s reads secrets in a way that names none" % (n, name))
            if "vars.SYNDEO_EXPECT_SIGNED" in c and name != "signing-config":
                found.append("release.yml:%d %s reads the signing configuration; only signing-config may" % (n, name))
            if SECRETS.search(c) and not scoped:
                found.append("release.yml:%d %s reads a secret or the signing configuration without the release-macos environment" % (n, name))
            if "--draft=false" in c:
                cond = [x.strip() for _, x in lines if x.startswith("    if:")]
                if cond != ["if: needs.verify.outputs.publish == 'true'"]:
                    found.append("release.yml:%d %s publishes a release without running only for a tag" % (n, name))
        if scoped:
            if any(c.strip() == "contents: write" for _, c in lines):
                found.append("release.yml: %s has the secrets and a token that can write" % name)
            stage = SIGNING_JOBS.get(name, (None, None))[1]
            want = "ci/check-signing-config.sh --stage %s" % stage if stage else "ci/check-signing-config.sh --github-output"
            gate = [n for n, c in lines if want in c]
            signs = [n for n, c in lines if SIGNS.search(c)]
            if not gate:
                found.append("release.yml: %s never runs ci/check-signing-config.sh (%s)" % (name, want.split(" ", 1)[1]))
            elif signs and min(signs) < min(gate):
                found.append("release.yml:%d %s signs before ci/check-signing-config.sh has run" % (min(signs), name))
            if name == "signing-config" and signs:
                found.append("release.yml:%d signing-config signs or notarizes; it may only decide" % min(signs))
        for n, c in lines:
            if "notarytool" in c:
                found.append("release.yml:%d %s calls notarytool directly; only ci/notarize.py may" % (n, name))
    for name in SIGNING_JOBS:
        if name not in jobs:
            found.append("release.yml: no %s job" % name)
    found += check_order(jobs)
    found += check_publication(jobs)
    return found


def first_line(lines, needle):
    """The line number of the first command holding NEEDLE, or None."""
    hits = [n for n, c in joined(lines) if needle in c]
    return min(hits) if hits else None


def check_order(jobs):
    """The macOS release's jobs, in order, and what passes between them."""
    found = []
    if not all(j in jobs for j in GRAPH):
        return ["release.yml: missing %s" % ", ".join(j for j in GRAPH if j not in jobs)]
    for name, want in GRAPH.items():
        needs = needs_of(jobs[name])
        if needs != sorted(want):
            found.append("release.yml: %s needs %s, not exactly %s" % (name, needs, ", ".join(sorted(want))))
    for name, lines in jobs.items():
        for n, c in lines:
            if c.startswith("    if:") and JOB_IFS.get(name) != c.strip():
                found.append("release.yml:%d %s has a job-level %r; only publish and rehearse may have one" % (n, name, c.strip()))
    # Provenance: one tarball and one Syndeo.app, each by its SHA-256, from
    # build-macos to package.
    b, nb, pk = jobs["build-macos"], jobs["notarize-binaries"], jobs["package"]
    if not any(c.strip() == "name: internal-macos-tarball" for _, c in b) or \
            not any("tarball-sha256: ${{ steps.tarball.outputs.sha256 }}" in c for _, c in b):
        found.append("release.yml: build-macos does not hand on the tarball (internal-macos-tarball) with its SHA-256")
    if not any(c.strip() == "name: internal-macos-app" for _, c in b) or \
            not any("app-sha256: ${{ steps.tarball.outputs.app-sha256 }}" in c for _, c in b):
        found.append("release.yml: build-macos does not hand on Syndeo.app (internal-macos-app) with its SHA-256")
    if not any("app-sha256: ${{ steps.tarball.outputs.app-sha256 }}" in c for _, c in nb):
        found.append("release.yml: notarize-binaries does not hand on Syndeo.app's SHA-256")
    checks = {}
    for name, lines, frm in (("notarize-binaries", nb, "build-macos"), ("package", pk, "notarize-binaries")):
        for what, artifact, var, path, output in (
                ("the tarball", "internal-macos-tarball", "WANT", "dist/$TARBALL", "tarball-sha256"),
                ("Syndeo.app", "internal-macos-app", "WANT_APP", "dist/$APPZIP", "app-sha256")):
            if not any(c.strip() == "name: %s" % artifact for _, c in lines):
                found.append("release.yml: %s does not take the %s artifact" % (name, artifact))
            upstream = "needs.%s.outputs.%s" % (frm, output)
            checked = [st for st in steps(lines)
                       if any(c.strip() == "%s: ${{ %s }}" % (var, upstream) for _, c in st)
                       and any('"$%s" "%s" | shasum -a 256 -c -' % (var, path) in c for _, c in st)]
            if not checked:
                found.append("release.yml: %s does not require %s's SHA-256 to be %s" % (name, what, upstream))
            else:
                checks[(name, what)] = min(n for n, _ in checked[0])
    if not any("needs.notarize-binaries.outputs.notarization" in c for _, c in pk):
        found.append("release.yml: package does not require the binaries' notarization")
    app = '"$RUNNER_TEMP/app/Syndeo.app"'
    unpack = '/usr/bin/ditto -x -k "dist/$APPZIP" "$RUNNER_TEMP/app"'
    staple = 'ci/sign-macos-pkg.sh staple-app "dist/$APPZIP" "$RUNNER_TEMP/app" diagnostics/stapling'
    build = first_line(pk, 'ci/package-macos-pkg.sh "$VERSION" ' + app)
    if first_line(pk, "ci/package-macos-pkg.sh") is None or build is None:
        found.append("release.yml: package does not build the package from $RUNNER_TEMP/app/Syndeo.app")
    else:
        sha = [checks.get(("package", w)) for w in ("the tarball", "Syndeo.app")]
        if None in sha or build < max(sha):
            found.append("release.yml: package builds the package before checking the tarball and Syndeo.app")
        stapled, unpacked = first_line(pk, staple), first_line(pk, unpack)
        if stapled is None or unpacked is None or build < max(stapled, unpacked) or \
                checks.get(("package", "Syndeo.app")) is None or min(stapled, unpacked) < checks[("package", "Syndeo.app")]:
            found.append("release.yml: package does not staple Syndeo.app (or, unsigned, unpack it) after checking it "
                         "and before building the package from it")
    # Apple: once per file, submit then wait, in these two jobs only.
    for name, lines in jobs.items():
        cmds = joined(lines)
        submits = [n for n, c in cmds if "ci/notarize.py submit" in c]
        waits = [n for n, c in cmds if "ci/notarize.py wait" in c]
        if name not in ("notarize-binaries", "package"):
            if submits or waits:
                found.append("release.yml: %s notarizes; only notarize-binaries and package may" % name)
            continue
        if len(submits) != 1 or len(waits) != 1:
            found.append("release.yml: %s submits %d times and waits %d times, not once each" % (name, len(submits), len(waits)))
        elif waits[0] < submits[0]:
            found.append("release.yml: %s waits before it submits" % name)
        if waits and not any('--deadline-minutes "$NOTARY_DEADLINE_MINUTES"' in c for n, c in cmds if n in waits):
            found.append("release.yml: %s waits without its NOTARY_DEADLINE_MINUTES" % name)
        timeout = [c.strip() for _, c in lines if c.startswith("    timeout-minutes:")]
        deadline = [c.strip() for _, c in lines if c.strip().startswith("NOTARY_DEADLINE_MINUTES:")]
        t = int(re.sub(r"\D", "", timeout[0])) if len(timeout) == 1 else None
        d = int(re.sub(r"\D", "", deadline[0])) if len(deadline) == 1 else None
        if t is None or d is None:
            found.append("release.yml: %s sets no timeout-minutes or no NOTARY_DEADLINE_MINUTES" % name)
        elif t > 355 or d > 300 or t - d < 30:
            found.append("release.yml: %s's deadline %d and timeout %d do not fit: timeout at most 355, deadline at most 300, "
                         "and 30 minutes between them" % (name, d, t))
    zipped = first_line(nb, "ci/notarize.py binaries-zip")
    submitted = first_line(nb, "ci/notarize.py submit")
    if zipped is None or submitted is None or zipped > submitted:
        found.append("release.yml: notarize-binaries does not make the binaries' ZIP before it submits")
    if first_line(nb, 'ci/notarize.py binaries-zip "dist/$TARBALL" "$VERSION" %s' % app) is None:
        found.append("release.yml: notarize-binaries' ZIP does not carry Syndeo.app with the seven")
    nb_unpack = first_line(nb, unpack)
    if nb_unpack is None or checks.get(("notarize-binaries", "Syndeo.app")) is None or \
            nb_unpack < checks[("notarize-binaries", "Syndeo.app")] or zipped is None or nb_unpack > zipped:
        found.append("release.yml: notarize-binaries does not unpack Syndeo.app after checking it and before the ZIP")
    # Every signed binary checked and run before Apple sees any of them: in
    # build-macos once they are signed, and in notarize-binaries before the
    # ZIP is made and anything is submitted. macOS kills a binary it refuses
    # as it starts, and a submission is not something to spend on one.
    run = "ci/verify-pkg.sh signed-binaries"
    signing, ran = first_line(b, "ci/sign-macos.sh"), first_line(b, run)
    if signing is None or ran is None or ran < signing:
        found.append("release.yml: build-macos does not run the signed binaries (%s) once it has signed them" % run)
    ran = first_line(nb, run)
    if ran is None or zipped is None or submitted is None or ran > zipped or ran > submitted:
        found.append("release.yml: notarize-binaries does not run the signed binaries (%s) before the ZIP and the submission" % run)
    run = 'ci/verify-pkg.sh signed-app %s "$VERSION" --tarball "dist/$TARBALL"' % app
    signing, ran = first_line(b, "ci/sign-macos.sh"), first_line(b, run)
    if signing is None or ran is None or ran < signing:
        found.append("release.yml: build-macos does not check the signed Syndeo.app and run its helper (%s) once it has signed it" % run)
    ran = first_line(nb, run)
    if ran is None or zipped is None or submitted is None or ran > zipped or ran > submitted:
        found.append("release.yml: notarize-binaries does not check the signed Syndeo.app (%s) before the ZIP and the submission" % run)
    order = [first_line(pk, x) for x in (staple, "ci/package-macos-pkg.sh", "ci/sign-macos-pkg.sh sign", "ci/notarize.py submit",
                                          "ci/notarize.py wait", "ci/sign-macos-pkg.sh finish", "ci/verify-pkg.sh inspect")]
    if None in order or order != sorted(order):
        found.append("release.yml: package does not staple the app, build, sign, submit, wait, staple and inspect, in that order")
    if first_line(pk, 'ci/verify-pkg.sh inspect "dist/$PKG" "$VERSION" --app %s' % app) is None:
        found.append("release.yml: package does not inspect the package against the Syndeo.app it was built from")
    # Stapling's diagnostics: written by finish to one directory, uploaded
    # only when the package job fails, from that directory alone, for at most
    # a week, and read by no other job. Nothing that inspects the package or
    # hands it on runs after a failure.
    finish = 'ci/sign-macos-pkg.sh finish "dist/$PKG" "$RUNNER_TEMP/pkg-state" notarization/package diagnostics/stapling'
    if not any(finish in c for _, c in joined(pk)):
        found.append("release.yml: package does not have ci/sign-macos-pkg.sh finish keep its diagnostics in diagnostics/stapling")
    kept = [st for st in steps(pk) if any(c.strip() == "name: internal-staple-diagnostics" for _, c in st)]
    if len(kept) != 1:
        found.append("release.yml: package keeps the stapling diagnostics in %d steps, not one" % len(kept))
    else:
        st = [c.strip() for _, c in kept[0]]
        days = [re.sub(r"\D", "", x) for x in st if x.startswith("retention-days:")]
        if [x for x in st if x.startswith("if:")] != ["if: failure()"] or \
                [x for x in st if x.startswith("path:")] != ["path: diagnostics/stapling/"] or \
                len(days) != 1 or not days[0] or int(days[0]) > 7:
            found.append("release.yml: package's stapling diagnostics are not kept only on failure, from diagnostics/stapling/ "
                         "alone, for at most 7 days")
    for name, lines in jobs.items():
        if name != "package" and any("internal-staple-diagnostics" in c for _, c in lines):
            found.append("release.yml: %s reads the stapling diagnostics; no job but package may name them" % name)
    for st in steps(pk):
        text = " ".join(c for _, c in st)
        if ("ci/verify-pkg.sh inspect" in text or "name: syndeo-aarch64-apple-darwin" in text) and \
                any(c.strip().startswith("if:") and re.search(r"always\(\)|failure\(\)|cancelled\(\)", c) for _, c in st):
            found.append("release.yml: package inspects the package, or hands it on, even after a failure")
    return found


def check(directory):
    found = []
    for name, fn in (("ci.yml", check_ci), ("release.yml", check_release)):
        path = os.path.join(directory, name)
        try:
            text = open(path).read()
        except OSError as e:
            found.append("%s: %s" % (name, e))
            continue
        found += fn(text)
    return found


def self_test(directory):
    ci = open(os.path.join(directory, "ci.yml")).read()
    rel = open(os.path.join(directory, "release.yml")).read()
    cases = wrong = 0

    def case(label, ci_text, rel_text, want):
        nonlocal cases, wrong
        # A check that cannot even read the workflow is a wrong case, not a crash.
        try:
            found = check_ci(ci_text) + check_release(rel_text)
        except Exception as e:
            found = None
            print("  WRONG %s: the check failed: %s: %s" % (label, e.__class__.__name__, e))
        cases += 1
        if found is None:
            wrong += 1
            return
        good = (not found) if want is None else any(want in f for f in found)
        if good:
            print("  ok    %s" % label)
        else:
            print("  WRONG %s: %s" % (label, found or "nothing found"))
            wrong += 1

    def swap(text, old, new, count=1):
        if text.count(old) < 1:
            raise SystemExit("self-test: %r is not in the workflow" % old)
        return text.replace(old, new, count)

    def job(text, name):
        """The text of one job, to change only inside it."""
        start = text.index("\n  %s:\n" % name) + 1
        m = re.compile(r"^  [A-Za-z0-9_-]+:\s*$", re.M).search(text, start + 1)
        return start, (m.start() if m else len(text))

    def in_job(text, name, old, new):
        a, b = job(text, name)
        return text[:a] + swap(text[a:b], old, new) + text[b:]

    def add_to_job(text, name, line):
        a, b = job(text, name)
        first = text.index("\n", a) + 1
        return text[:first] + line + "\n" + text[first:]

    print("\n  the workflows as they are")
    case("ci.yml and release.yml pass", ci, rel, None)

    print("\n  ci.yml, changed")
    case("a secret in a pull-request step", swap(ci, "      - run: bash ci/test-install.sh",
         "      - run: bash ci/test-install.sh\n        env:\n          X: ${{ secrets.MACOS_TEAM_ID }}"), rel, "references a secret")
    case("an environment on a pull-request job", add_to_job(ci, "scripts", "    environment: release-macos"), rel, "declares an environment")
    case("pull_request_target", swap(ci, "  pull_request:\n", "  pull_request_target:\n"), rel, "pull_request_target")

    print("\n  release.yml, changed")
    case("a secret in a Linux build", ci, add_to_job(rel, "build-linux", "    env:\n      X: ${{ secrets.MACOS_TEAM_ID }}"),
         "build-linux reads a secret")
    case("the signing configuration read by the checksums job", ci,
         add_to_job(rel, "checksums", "    env:\n      SYNDEO_EXPECT_SIGNED: ${{ vars.SYNDEO_EXPECT_SIGNED }}"), "checksums reads the signing configuration")
    case("the environment removed from the macOS build", ci, in_job(rel, "build-macos", "    environment: release-macos\n", ""),
         "build-macos reads a secret or the signing configuration without")
    case("the environment on the package install as well", ci, add_to_job(rel, "pkg-install", "    environment: release-macos"),
         "pkg-install declares")
    case("another environment on the macOS build", ci, in_job(rel, "build-macos", "    environment: release-macos", "    environment: release"),
         "build-macos declares")
    case("a secret at the top", ci, swap(rel, "env:\n  CARGO_TERM_COLOR: always", "env:\n  CARGO_TERM_COLOR: always\n  X: ${{ secrets.MACOS_TEAM_ID }}"),
         "outside any job")
    case("a pull_request trigger", ci, swap(rel, "on:\n", "on:\n  pull_request:\n"), "runs on pull_request")
    case("a workflow_run trigger", ci, swap(rel, "on:\n", "on:\n  workflow_run:\n    workflows: [CI]\n"), "runs on workflow_run")
    case("a top-level token that can write", ci, swap(rel, "permissions:\n  contents: read\n", "permissions:\n  contents: write\n"),
         "not read-only")
    case("the macOS build given a token that can write", ci, add_to_job(rel, "build-macos", "    permissions:\n      contents: write"),
         "a token that can write")
    case("the gate removed from the macOS build", ci, in_job(rel, "build-macos", "ci/check-signing-config.sh --stage binary-sign", "true"),
         "build-macos never runs ci/check-signing-config.sh")
    case("the whole gate removed from signing-config", ci, in_job(rel, "signing-config", "ci/check-signing-config.sh --github-output", "true"),
         "signing-config never runs ci/check-signing-config.sh")
    moved = in_job(rel, "build-macos", "      - uses: actions/checkout@v4\n",
                   "      - uses: actions/checkout@v4\n      - name: Sign first\n        run: ./ci/sign-macos.sh target\n")
    case("a signing step before the gate", ci, moved, "signs before ci/check-signing-config.sh")
    case("a release made public outside the tag-only job", ci,
         in_job(rel, "rehearse", "      - name: Deleted, whatever happened, and nothing left\n",
                "      - run: gh release edit x --draft=false\n      - name: Deleted, whatever happened, and nothing left\n"),
         "publishes a release without running only for a tag")


    print("\n  release.yml's secrets, one share per job")
    case("the notary key given to build-macos", ci,
         in_job(rel, "build-macos", "          TEAM_ID: ${{ secrets.MACOS_TEAM_ID }}\n",
                "          TEAM_ID: ${{ secrets.MACOS_TEAM_ID }}\n          NOTARY_KEY: ${{ secrets.MACOS_NOTARY_KEY_BASE64 }}\n"),
         "build-macos reads secrets.MACOS_NOTARY_KEY_BASE64, which it is not approved for")
    case("the Application certificate given to package", ci,
         add_to_job(rel, "package", "    env:\n      X: ${{ secrets.MACOS_CERTIFICATE_P12_BASE64 }}"),
         "package reads secrets.MACOS_CERTIFICATE_P12_BASE64, which it is not approved for")
    case("the Installer certificate given to notarize-binaries", ci,
         add_to_job(rel, "notarize-binaries", "    env:\n      X: ${{ secrets.MACOS_INSTALLER_CERTIFICATE_P12_BASE64 }}"),
         "notarize-binaries reads secrets.MACOS_INSTALLER_CERTIFICATE_P12_BASE64, which it is not approved for")
    case("the signing configuration read by package", ci,
         add_to_job(rel, "package", "    env:\n      X: ${{ vars.SYNDEO_EXPECT_SIGNED }}"),
         "package reads the signing configuration; only signing-config may")
    case("the environment removed from notarize-binaries", ci,
         in_job(rel, "notarize-binaries", "    environment: release-macos\n", ""), "notarize-binaries must declare environment")
    case("signing-config made to sign", ci,
         in_job(rel, "signing-config", "        run: ./ci/check-signing-config.sh --github-output\n",
                "        run: ./ci/check-signing-config.sh --github-output\n      - run: ./ci/sign-macos.sh target\n"),
         "signing-config signs or notarizes")
    case("notarize-binaries gating as another stage", ci,
         in_job(rel, "notarize-binaries", "--stage binary-notarize", "--stage package"),
         "notarize-binaries never runs ci/check-signing-config.sh")
    a, b = job(rel, "notarize-binaries")
    text = rel[a:b]
    gate_at = text.index("      - name: This job's share of the signing configuration\n")
    after_gate = text.index("      - uses: actions/download-artifact@v4\n")
    submit_at = text.index("      - name: Submit the binaries to Apple, once\n")
    wait_at = text.index("      - name: Wait for Apple's verdict on the binaries\n")
    late_gate = text[:gate_at] + text[after_gate:wait_at] + text[gate_at:after_gate] + text[wait_at:]
    case("notarize-binaries submitting before its gate", ci, rel[:a] + late_gate + rel[b:],
         "notarize-binaries signs before ci/check-signing-config.sh")
    case("notarytool called directly", ci,
         in_job(rel, "package", "        run: python3 -I ci/notarize.py submit \"dist/$PKG\" --record notarization/package\n",
                "        run: xcrun notarytool submit \"dist/$PKG\" --wait\n"),
         "package calls notarytool directly")

    print("\n  release.yml's macOS jobs, in order, on one tarball")
    for name, line, short in (
            ("notarize-binaries", "    needs: [verify, signing-config, build-macos]", "    needs: [verify, signing-config]"),
            ("package", "    needs: [verify, signing-config, build-macos, notarize-binaries]", "    needs: [verify, signing-config, build-macos]"),
            ("pkg-install", "    needs: [verify, build-macos, notarize-binaries, package]", "    needs: [verify, build-macos, notarize-binaries]"),
            ("pkg-install", "    needs: [verify, build-macos, notarize-binaries, package]", "    needs: [verify, build-macos, package]"),
            ("checksums", "    needs: [verify, build-linux, build-macos, notarize-binaries, package]",
             "    needs: [verify, build-linux, build-macos, package]"),
            ("checksums", "    needs: [verify, build-linux, build-macos, notarize-binaries, package]",
             "    needs: [verify, build-linux, build-macos, notarize-binaries]")):
        case("%s with %s" % (name, short.strip()), ci, in_job(rel, name, line, short), "%s needs" % name)
    case("package running even when the binaries' notarization failed", ci,
         add_to_job(rel, "package", "    if: always()"), "package has a job-level 'if: always()'")
    case("pkg-install running whatever happened", ci,
         add_to_job(rel, "pkg-install", "    if: ${{ !cancelled() }}"), "pkg-install has a job-level")
    case("build-macos not handing on the tarball", ci,
         in_job(rel, "build-macos", "          name: internal-macos-tarball\n", "          name: macos-tarball\n"),
         "build-macos does not hand on the tarball")
    case("notarize-binaries not checking the tarball's SHA-256", ci,
         in_job(rel, "notarize-binaries", "          printf '%s  %s\\n' \"$WANT\" \"dist/$TARBALL\" | shasum -a 256 -c -\n", ""),
         "notarize-binaries does not require the tarball's SHA-256")
    case("notarize-binaries not checking Syndeo.app's SHA-256", ci,
         in_job(rel, "notarize-binaries", "          printf '%s  %s\\n' \"$WANT_APP\" \"dist/$APPZIP\" | shasum -a 256 -c -\n", ""),
         "notarize-binaries does not require Syndeo.app's SHA-256")
    case("build-macos not handing on Syndeo.app", ci,
         in_job(rel, "build-macos", "          name: internal-macos-app\n", "          name: macos-app\n"),
         "build-macos does not hand on Syndeo.app")
    case("notarize-binaries not handing on Syndeo.app's SHA-256", ci,
         in_job(rel, "notarize-binaries", "      app-sha256: ${{ steps.tarball.outputs.app-sha256 }}\n", ""),
         "notarize-binaries does not hand on Syndeo.app's SHA-256")
    case("package checking Syndeo.app against build-macos, not against what was notarized", ci,
         in_job(rel, "package", "WANT_APP: ${{ needs.notarize-binaries.outputs.app-sha256 }}",
                "WANT_APP: ${{ needs.build-macos.outputs.app-sha256 }}"),
         "package does not require Syndeo.app's SHA-256")
    case("package not checking Syndeo.app's SHA-256", ci,
         in_job(rel, "package", "          printf '%s  %s\\n' \"$WANT_APP\" \"dist/$APPZIP\" | shasum -a 256 -c -\n", ""),
         "package does not require Syndeo.app's SHA-256")
    case("notarize-binaries' ZIP without Syndeo.app", ci,
         in_job(rel, "notarize-binaries", 'binaries-zip "dist/$TARBALL" "$VERSION" "$RUNNER_TEMP/app/Syndeo.app"',
                'binaries-zip "dist/$TARBALL" "$VERSION" "$RUNNER_TEMP/other/Syndeo.app"'),
         "notarize-binaries' ZIP does not carry Syndeo.app")
    case("package not stapling Syndeo.app", ci,
         in_job(rel, "package", 'run: ./ci/sign-macos-pkg.sh staple-app "dist/$APPZIP" "$RUNNER_TEMP/app" diagnostics/stapling', "run: true"),
         "package does not staple Syndeo.app")
    case("build-macos not checking the signed Syndeo.app", ci,
         in_job(rel, "build-macos", '        run: bash ci/verify-pkg.sh signed-app "$RUNNER_TEMP/app/Syndeo.app" "$VERSION" --tarball "dist/$TARBALL"\n',
                "        run: true\n"),
         "build-macos does not check the signed Syndeo.app")
    case("notarize-binaries not checking the signed Syndeo.app", ci,
         in_job(rel, "notarize-binaries", '          bash ci/verify-pkg.sh signed-app "$RUNNER_TEMP/app/Syndeo.app" "$VERSION" --tarball "dist/$TARBALL"\n',
                "          true\n"),
         "notarize-binaries does not check the signed Syndeo.app")
    case("package inspecting without the bundle it was built from", ci,
         in_job(rel, "package", '--app "$RUNNER_TEMP/app/Syndeo.app"', '--app "$RUNNER_TEMP/other/Syndeo.app"'),
         "package does not inspect the package against the Syndeo.app")
    case("package checking the tarball against build-macos, not against what was notarized", ci,
         in_job(rel, "package", "WANT: ${{ needs.notarize-binaries.outputs.tarball-sha256 }}",
                "WANT: ${{ needs.build-macos.outputs.tarball-sha256 }}"),
         "package does not require the tarball's SHA-256")
    case("package not requiring the binaries' notarization", ci,
         in_job(rel, "package", "          NOTARIZATION: ${{ needs.notarize-binaries.outputs.notarization }}\n", ""),
         "package does not require the binaries' notarization")
    case("package building from another bundle", ci,
         in_job(rel, "package", './ci/package-macos-pkg.sh "$VERSION" "$RUNNER_TEMP/app/Syndeo.app"',
                './ci/package-macos-pkg.sh "$VERSION" "$RUNNER_TEMP/other/Syndeo.app"'),
         "package does not build the package from $RUNNER_TEMP/app/Syndeo.app")
    a, b = job(rel, "package")
    text = rel[a:b]
    sha_at = text.index("      - name: The tarball and Syndeo.app Apple accepted, byte for byte\n")
    staple_at = text.index("      - name: Staple Syndeo.app, which Apple accepted with the binaries, and ask Gatekeeper\n")
    build_at = text.index("      - name: The package, from that Syndeo.app\n")
    sign_at = text.index("      - name: Sign the package\n")
    early = text[:sha_at] + text[build_at:sign_at] + text[sha_at:build_at] + text[sign_at:]
    case("package building before it checks the tarball and the bundle", ci, rel[:a] + early + rel[b:],
         "package builds the package before checking the tarball and Syndeo.app")
    unstapled = text[:staple_at] + text[build_at:sign_at] + text[staple_at:build_at] + text[sign_at:]
    case("package building before it staples the bundle", ci, rel[:a] + unstapled + rel[b:],
         "package does not staple Syndeo.app (or, unsigned, unpack it) after checking it and before building")
    case("a second submission of the binaries", ci,
         in_job(rel, "notarize-binaries", "      - name: Wait for Apple's verdict on the binaries\n",
                "      - run: python3 -I ci/notarize.py submit \"$RUNNER_TEMP/notarize.zip\" --record notarization/again\n"
                "      - name: Wait for Apple's verdict on the binaries\n"),
         "notarize-binaries submits 2 times")
    submit_at = text.index("      - name: Submit the package to Apple, once\n")
    wait_at = text.index("      - name: Wait for Apple's verdict on the package\n")
    finish_at = text.index("      - name: Staple it, once Apple accepted it, and ask Gatekeeper\n")
    swapped = text[:submit_at] + text[wait_at:finish_at] + text[submit_at:wait_at] + text[finish_at:]
    case("package waiting before it submits", ci, rel[:a] + swapped + rel[b:], "package waits before it submits")
    unsigned_at = text.index("      - name: The package, unsigned, as the configuration decided\n")
    staple_early = text[:submit_at] + text[finish_at:unsigned_at] + text[submit_at:finish_at] + text[unsigned_at:]
    case("package stapling before Apple's verdict", ci, rel[:a] + staple_early + rel[b:],
         "package does not staple the app, build, sign, submit, wait, staple and inspect, in that order")
    case("build-macos notarizing", ci,
         in_job(rel, "build-macos", "      - name: The tarball\n",
                "      - run: python3 -I ci/notarize.py submit dist/x.zip --record r\n      - name: The tarball\n"),
         "build-macos notarizes")
    case("the stapling diagnostics kept whatever happened", ci,
         in_job(rel, "package", "        if: failure()\n", "        if: always()\n"),
         "stapling diagnostics are not kept only on failure")
    case("the stapling diagnostics kept from RUNNER_TEMP", ci,
         in_job(rel, "package", "          path: diagnostics/stapling/\n", "          path: ${{ runner.temp }}\n"),
         "stapling diagnostics are not kept only on failure")
    case("the stapling diagnostics kept for 90 days", ci,
         in_job(rel, "package", "          retention-days: 7\n", "          retention-days: 90\n"),
         "stapling diagnostics are not kept only on failure")
    case("another job reading the stapling diagnostics", ci,
         add_to_job(rel, "pkg-install", "    env:\n      X: internal-staple-diagnostics"),
         "pkg-install reads the stapling diagnostics")
    case("finish without its diagnostics directory", ci,
         in_job(rel, "package", " notarization/package diagnostics/stapling\n", " notarization/package\n"),
         "package does not have ci/sign-macos-pkg.sh finish keep its diagnostics")
    case("the package inspected even after a failure", ci,
         in_job(rel, "package", "      - name: Inspect the package without installing it\n",
                "      - name: Inspect the package without installing it\n        if: always()\n"),
         "package inspects the package, or hands it on, even after a failure")
    case("the package handed on even after a failure", ci,
         in_job(rel, "package", "      - uses: actions/upload-artifact@v4\n        with:\n          name: syndeo-aarch64-apple-darwin\n",
                "      - uses: actions/upload-artifact@v4\n        if: ${{ !cancelled() }}\n        with:\n          name: syndeo-aarch64-apple-darwin\n"),
         "package inspects the package, or hands it on, even after a failure")
    signed_run = '        run: bash ci/verify-pkg.sh signed-binaries "dist/$TARBALL" "$VERSION"\n'
    case("build-macos not running the signed binaries", ci,
         in_job(rel, "build-macos", signed_run, "        run: true\n"),
         "build-macos does not run the signed binaries")
    a, b = job(rel, "build-macos")
    text = rel[a:b]
    sign_at = text.index("      - name: Sign the binaries, then Syndeo.app\n")
    check_at = text.index("      - name: Check the signed binaries in the tarball, and run each of them\n")
    sum_at = text.index("      - name: Check the signed Syndeo.app, and run its helper\n")
    early = text[:sign_at] + text[check_at:sum_at] + text[sign_at:check_at] + text[sum_at:]
    case("build-macos running the binaries before it signs them", ci, rel[:a] + early + rel[b:],
         "build-macos does not run the signed binaries")
    case("notarize-binaries not running the signed binaries", ci,
         in_job(rel, "notarize-binaries", signed_run, "        run: true\n"),
         "notarize-binaries does not run the signed binaries")
    a, b = job(rel, "notarize-binaries")
    text = rel[a:b]
    check_at = text.index("      - name: Check and run the signed binaries again, before Apple sees them\n")
    zip_at = text.index("      - name: The ZIP of exactly the seven executables and Syndeo.app\n")
    wait_at = text.index("      - name: Wait for Apple's verdict on the binaries\n")
    late = text[:check_at] + text[zip_at:wait_at] + text[check_at:zip_at] + text[wait_at:]
    case("notarize-binaries running the binaries only after it submits them", ci, rel[:a] + late + rel[b:],
         "notarize-binaries does not run the signed binaries")
    case("notarize-binaries without a time limit", ci,
         in_job(rel, "notarize-binaries", "    timeout-minutes: 350\n", ""), "notarize-binaries sets no timeout-minutes")
    case("package allowed GitHub's whole 360 minutes", ci,
         in_job(rel, "package", "    timeout-minutes: 350\n", "    timeout-minutes: 360\n"), "package's deadline 300 and timeout 360")
    case("a deadline that leaves no time to clean up", ci,
         in_job(rel, "notarize-binaries", 'NOTARY_DEADLINE_MINUTES: "300"', 'NOTARY_DEADLINE_MINUTES: "340"'),
         "notarize-binaries's deadline 340 and timeout 350")
    case("a wait without the deadline", ci,
         in_job(rel, "package", ' --deadline-minutes "$NOTARY_DEADLINE_MINUTES"', ' --deadline-minutes 900'),
         "package waits without its NOTARY_DEADLINE_MINUTES")

    print("\n  release.yml's publication, changed")
    deps = ["verify", "signing-config", "build-macos", "notarize-binaries", "package", "pkg-install", "checksums"]
    needs = "    needs: [%s]" % ", ".join(deps)
    for name in ("publish", "rehearse"):
        for dep in deps:
            fewer = "    needs: [%s]" % ", ".join(d for d in deps if d != dep)
            case("%s without %s among its needs" % (name, dep), ci, in_job(rel, name, needs, fewer),
                 "%s needs" % name)
        case("%s needing build-linux as well" % name, ci,
             in_job(rel, name, needs, "    needs: [%s, build-linux]" % ", ".join(deps)), "%s needs" % name)
    case("publish's release not created as a draft", ci, in_job(rel, "publish", "--draft --verify-tag", "--verify-tag"),
         "publish creates a release that is not a draft")
    case("rehearse's release not created as a draft", ci, in_job(rel, "rehearse", "--draft --target", "--target"),
         "rehearse creates a release that is not a draft")
    case("publish's draft created without --verify-tag", ci, in_job(rel, "publish", "--draft --verify-tag", "--draft"),
         "without --verify-tag")
    a, b = job(rel, "publish")
    text = rel[a:b]
    check_at = text.index("      - name: The draft, downloaded and checked\n")
    public_at = text.index("      - name: Published, only now\n")
    reordered = text[:check_at] + text[public_at:] + text[check_at:public_at]
    case("publish made public before its draft is checked", ci, rel[:a] + reordered + rel[b:],
         "public before checking its draft")
    case("publish's check of its draft removed", ci,
         in_job(rel, "publish", "bash ci/verify-pkg.sh release check", "true"), "never checks its draft")
    case("rehearse's cleanup not run always", ci, in_job(rel, "rehearse", "        if: always()\n", ""),
         "no if: always() step that deletes its draft")
    case("rehearse's cleanup not deleting the draft", ci, in_job(rel, "rehearse", 'gh release delete "$TAG" --yes', "true"),
         "no if: always() step that deletes its draft")
    gone = ('          if gh release view "$TAG" >/dev/null 2>&1; then\n'
            '            echo "::error::the dry-run draft $TAG is still there"\n'
            '            exit 1\n'
            '          fi\n')
    case("rehearse's cleanup not checking the draft is gone", ci, in_job(rel, "rehearse", gone, ""),
         "does not fail when its draft remains")
    tag = ('          if gh api "repos/$GITHUB_REPOSITORY/git/ref/tags/$TAG" >/dev/null 2>&1; then\n'
           '            echo "::error::a tag $TAG exists; a dry run must never create one"\n'
           '            exit 1\n'
           '          fi\n')
    case("rehearse's cleanup not checking for a tag", ci, in_job(rel, "rehearse", tag, ""),
         "does not fail when a tag of its draft's name exists")
    case("rehearse's cleanup finding the tag but not failing", ci,
         in_job(rel, "rehearse", tag, tag.replace("exit 1", "exit 0")),
         "does not fail when a tag of its draft's name exists")
    case("rehearse made to publish its draft", ci,
         in_job(rel, "rehearse", "      - name: Deleted, whatever happened, and nothing left\n",
                '      - name: Publish\n        run: gh release edit "$TAG" --draft=false --latest\n'
                "      - name: Deleted, whatever happened, and nothing left\n"),
         "rehearse publishes a release; only publish may")
    for name in ("verify", "build-linux", "pkg-install", "checksums"):
        case("%s given a token that can write" % name, ci, add_to_job(rel, name, "    permissions:\n      contents: write"),
             "%s raises the token to contents: write" % name)
    case("a job asking for write-all", ci, add_to_job(rel, "checksums", "    permissions: write-all"), "asks for write-all")
    print("\n  %d cases, %d wrong\n" % (cases, wrong))
    return wrong == 0


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    directory = os.path.join(here, "..", ".github", "workflows")
    args = sys.argv[1:]
    if args == ["--self-test"]:
        return 0 if self_test(directory) else 1
    if len(args) == 1:
        directory = args[0]
    elif args:
        print("usage: check-workflows.py [WORKFLOW-DIR] | --self-test", file=sys.stderr)
        return 2
    found = check(directory)
    for f in found:
        print("check-workflows: %s" % f)
    if found:
        return 1
    print("check-workflows: ci.yml reads no secret and no environment; in release.yml only signing-config, build-macos, "
          "notarize-binaries and package, behind release-macos, read secrets, each only its own share, each gating first; "
          "the macOS release runs build, notarize the binaries and Syndeo.app, package, in order, on one tarball and one "
          "Syndeo.app by their SHA-256s, runs every signed binary and checks the signed bundle before Apple sees any of them, "
          "staples the bundle before the package is built from it, with one "
          "submission per file inside each job's time; every release is created a draft, publish checks it before making "
          "it public, and rehearse only deletes its own")
    return 0


if __name__ == "__main__":
    sys.exit(main())
