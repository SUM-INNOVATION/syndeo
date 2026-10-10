"""Check where the workflows let signing secrets go.

    python3 -I ci/check-workflows.py [WORKFLOW-DIR]
    python3 -I ci/check-workflows.py --self-test

ci.yml runs on every pull request, so it may reference no `secrets.` and no
`environment:` at all, and may not run on pull_request_target, which would
hand a fork's code the repository's secrets.

release.yml holds the signing and notarization secrets, and only one job may
see them: the macOS build, which declares the protected release-macos
environment and so waits for its reviewers. Checked:
- it runs on tags and workflow_dispatch, never on a pull request or another
  workflow's run;
- nothing outside the jobs reads a secret or the signing configuration;
- `secrets.` and `vars.SYNDEO_EXPECT_SIGNED` appear only in a job that
  declares `environment: release-macos`, and only build-macos declares an
  environment;
- that job cannot write to the repository: the token everything runs with
  is read-only at the top, and it does not raise it;
- in that job, ci/check-signing-config.sh runs before anything signs;
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
SIGNS = re.compile(r"sign-macos(-pkg)?\.sh|\bproductsign\b|\bcodesign\b|notarytool")


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
    """The jobs a job needs, sorted, or None if not written as [a, b]."""
    for n, c in lines:
        m = re.match(r"^    needs:\s*(.*)$", c)
        if m:
            v = m.group(1).strip()
            if v.startswith("[") and v.endswith("]"):
                return sorted(x.strip() for x in v[1:-1].split(",") if x.strip())
            return None
    return []


DRAFT = re.compile(r"(?<!\S)--draft(?!\S)")
PUBLISHES = re.compile(r"--draft[= ]false|draft=false|\"draft\":\s*false|gh release edit")
RELEASE_NEEDS = ["build-macos", "checksums", "pkg-install", "verify"]


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
            found.append("release.yml: %s needs %s, not exactly verify, build-macos, pkg-install and checksums"
                         % (name, "something it does not write as [a, b]" if needs is None else (needs or "nothing")))
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
        protected = any(c == "environment: release-macos" for _, c in envs)
        for n, c in envs:
            if c != "environment: release-macos" or name != "build-macos":
                found.append("release.yml:%d %s declares %r; only build-macos may, as release-macos" % (n, name, c))
        for n, c in lines:
            if (SECRETS.search(c) or "vars.SYNDEO_EXPECT_SIGNED" in c) and not protected:
                found.append("release.yml:%d %s reads a secret or the signing configuration without the release-macos environment" % (n, name))
            if "--draft=false" in c:
                cond = [x.strip() for _, x in lines if x.startswith("    if:")]
                if cond != ["if: needs.verify.outputs.publish == 'true'"]:
                    found.append("release.yml:%d %s publishes a release without running only for a tag" % (n, name))
        if protected:
            if any(c.strip() == "contents: write" for _, c in lines):
                found.append("release.yml: %s has the secrets and a token that can write" % name)
            gate = [n for n, c in lines if "ci/check-signing-config.sh" in c]
            signs = [n for n, c in lines if SIGNS.search(c)]
            if not gate:
                found.append("release.yml: %s never runs ci/check-signing-config.sh" % name)
            elif signs and min(signs) < min(gate):
                found.append("release.yml:%d %s signs before ci/check-signing-config.sh has run" % (min(signs), name))
    if "build-macos" not in jobs:
        found.append("release.yml: no build-macos job")
    found += check_publication(jobs)
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
        found = check_ci(ci_text) + check_release(rel_text)
        cases += 1
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
         add_to_job(rel, "checksums", "    env:\n      SYNDEO_EXPECT_SIGNED: ${{ vars.SYNDEO_EXPECT_SIGNED }}"), "checksums reads a secret")
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
    case("the gate removed from the macOS build", ci, in_job(rel, "build-macos", "ci/check-signing-config.sh --github-output", "true"),
         "never runs ci/check-signing-config.sh")
    moved = in_job(rel, "build-macos", "      - uses: actions/checkout@v4\n",
                   "      - uses: actions/checkout@v4\n      - name: Sign first\n        run: ./ci/sign-macos.sh target\n")
    case("a signing step before the gate", ci, moved, "signs before ci/check-signing-config.sh")
    case("a release made public outside the tag-only job", ci,
         in_job(rel, "rehearse", "      - name: Deleted, whatever happened, and nothing left\n",
                "      - run: gh release edit x --draft=false\n      - name: Deleted, whatever happened, and nothing left\n"),
         "publishes a release without running only for a tag")

    print("\n  release.yml's publication, changed")
    needs = "    needs: [verify, build-macos, pkg-install, checksums]"
    for name in ("publish", "rehearse"):
        for dep in ("verify", "build-macos", "pkg-install", "checksums"):
            fewer = "    needs: [%s]" % ", ".join(d for d in ("verify", "build-macos", "pkg-install", "checksums") if d != dep)
            case("%s without %s among its needs" % (name, dep), ci, in_job(rel, name, needs, fewer),
                 "%s needs" % name)
        case("%s needing build-linux as well" % name, ci,
             in_job(rel, name, needs, "    needs: [verify, build-linux, build-macos, pkg-install, checksums]"), "%s needs" % name)
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
    print("check-workflows: ci.yml reads no secret and no environment; in release.yml only build-macos, behind release-macos, does, "
          "and it runs the gate before signing; every release is created a draft, publish checks it before making it public, "
          "and rehearse only deletes its own")
    return 0


if __name__ == "__main__":
    sys.exit(main())
