#!/usr/bin/env python3
"""Syndeo.app's provisioning profile, and the one entitlement it authorizes.

  profile.py entitlements <team>
      the entitlements Syndeo.app's main executable is signed with, as an XML
      property list
  profile.py check <profile> <team> [--certificate <der>] [--now <time>]
      <profile> is a Developer ID provisioning profile for Syndeo.app of
      <team> that authorizes Apple's browser passkey entitlement, and has not
      expired (at <time>, YYYY-MM-DDTHH:MM:SSZ, if given); with
      --certificate, it lists that signing certificate, DER
  profile.py signed <entitlements.xml> <team>
      <entitlements.xml>, what `codesign -d --entitlements - --xml` printed
      for Syndeo.app, is exactly what `entitlements <team>` gives
  profile.py forge <out> <team> [--certificate <der>] [--without-passkeys]
                  [--expired] [--app-id <id>] [--devices]
      for self-tests only: a stand-in profile, CMS-signed with a throwaway
      key, saying what a real one would, or with the one thing named wrong
  profile.py --self-test

Syndeo.app claims com.apple.developer.web-browser.public-key-credential,
which lets a web browser make passkey and security-key requests for any site
(macOS 13.3 and later). Apple grants it to a team as a managed capability;
macOS honours it only where an embedded provisioning profile,
Contents/embedded.provisionprofile, authorizes it for the app's App ID. So
the main executable claims exactly three entitlements: that one, and the App
ID and team that tie it to the profile (com.apple.application-identifier,
com.apple.developer.team-identifier), as Xcode signs any Mac app that has a
profile. Its helper, and every command-line tool, claims none.

A profile is a property list in a CMS signature. It is read with
`security cms -D` on macOS, and `openssl cms -verify -noverify` elsewhere:
this checks what it says, not who signed it; macOS checks Apple's signature
on it when Syndeo.app starts. What it has to say, exactly:
  - Platform: OSX alone;
  - TeamIdentifier: the team alone;
  - Entitlements: com.apple.application-identifier <team>.com.sum.syndeo,
    com.apple.developer.team-identifier <team>, and the browser passkey
    entitlement, true;
  - ProvisionsAllDevices, true, and no ProvisionedDevices: a Developer ID
    profile, not a development one tied to devices;
  - ExpirationDate: after now;
  - with --certificate: DeveloperCertificates lists that certificate.
It prints what is wrong, one line each, and never the profile.
"""
import datetime
import os
import plistlib
import shutil
import subprocess
import sys
import tempfile

BUNDLE_ID = "com.sum.syndeo"
PASSKEYS = "com.apple.developer.web-browser.public-key-credential"
APP_ID = "com.apple.application-identifier"
TEAM_KEY = "com.apple.developer.team-identifier"


def team_valid(team):
    return len(team) == 10 and all(c in "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789" for c in team)


def entitlements(team):
    """What Syndeo.app's main executable claims, exactly."""
    return {APP_ID: f"{team}.{BUNDLE_ID}", TEAM_KEY: team, PASSKEYS: True}


def decode(path):
    """The property list inside the profile at path, as bytes, or None."""
    if sys.platform == "darwin":
        command = ["/usr/bin/security", "cms", "-D", "-i", path]
    else:
        command = ["openssl", "cms", "-verify", "-noverify", "-inform", "DER", "-in", path]
    try:
        done = subprocess.run(command, capture_output=True, timeout=60)
    except (OSError, subprocess.TimeoutExpired):
        return None
    return done.stdout if done.returncode == 0 and done.stdout else None


def problems(payload, team, certificate=None, now=None):
    """What keeps payload, a profile's property list, from being Syndeo.app's
    Developer ID profile of team with the passkey entitlement; [] if nothing."""
    now = now or datetime.datetime.now(datetime.timezone.utc)
    found = []
    try:
        profile = plistlib.loads(payload)
    except Exception:
        return ["it is not a property list"]
    if not isinstance(profile, dict):
        return ["it is not a dictionary"]
    if profile.get("Platform") != ["OSX"]:
        found.append(f"its platform is {profile.get('Platform')!r}, not macOS alone")
    if profile.get("TeamIdentifier") != [team]:
        found.append(f"its team is {profile.get('TeamIdentifier')!r}, not {team} alone")
    granted = profile.get("Entitlements")
    if not isinstance(granted, dict):
        found.append("it authorizes no entitlements")
        granted = {}
    if granted.get(APP_ID) != f"{team}.{BUNDLE_ID}":
        found.append(f"its App ID is {granted.get(APP_ID)!r}, not {team}.{BUNDLE_ID}")
    if granted.get(TEAM_KEY) != team:
        found.append(f"its team entitlement is {granted.get(TEAM_KEY)!r}, not {team}")
    if granted.get(PASSKEYS) is not True:
        found.append(f"it does not authorize {PASSKEYS}")
    if profile.get("ProvisionsAllDevices") is not True or "ProvisionedDevices" in profile:
        found.append("it is not a Developer ID profile: it is tied to devices, or not provisioned for all")
    expires = profile.get("ExpirationDate")
    if not isinstance(expires, datetime.datetime):
        found.append("it has no expiration date")
    else:
        if expires.tzinfo is None:
            expires = expires.replace(tzinfo=datetime.timezone.utc)
        if expires <= now:
            found.append(f"it expired at {expires.isoformat()}")
    if certificate is not None:
        listed = profile.get("DeveloperCertificates")
        if not isinstance(listed, list) or certificate not in listed:
            found.append("it does not list the certificate Syndeo.app is signed with")
    return found


def parse_now(text):
    return datetime.datetime.strptime(text, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=datetime.timezone.utc)


def check(argv):
    if len(argv) < 2:
        return usage()
    path, team, rest = argv[0], argv[1], argv[2:]
    certificate = now = None
    while rest:
        if rest[0] == "--certificate" and len(rest) >= 2:
            with open(rest[1], "rb") as f:
                certificate = f.read()
        elif rest[0] == "--now" and len(rest) >= 2:
            now = parse_now(rest[1])
        else:
            return usage()
        rest = rest[2:]
    if not team_valid(team):
        print("profile.py: the team is not ten capital letters and digits")
        return 1
    payload = decode(path)
    if payload is None:
        print("profile.py: the profile is not a CMS-signed property list")
        return 1
    found = problems(payload, team, certificate, now)
    for problem in found:
        print(f"profile.py: the provisioning profile: {problem}")
    if not found:
        print(f"profile.py: a Developer ID profile for {team}.{BUNDLE_ID}, authorizing {PASSKEYS}")
    return 1 if found else 0


def signed(argv):
    if len(argv) != 2 or not team_valid(argv[1]):
        return usage()
    with open(argv[0], "rb") as f:
        data = f.read()
    try:
        have = plistlib.loads(data) if data.strip() else {}
    except Exception:
        have = None
    if have != entitlements(argv[1]):
        print(f"profile.py: Syndeo.app's entitlements are not exactly {sorted(entitlements(argv[1]))}")
        return 1
    return 0


def forge(argv):
    """A stand-in profile for self-tests, signed with a throwaway key."""
    if len(argv) < 2 or not team_valid(argv[1]):
        return usage()
    out, team, rest = argv[0], argv[1], argv[2:]
    granted = entitlements(team)
    profile = {
        "Platform": ["OSX"],
        "TeamIdentifier": [team],
        "ProvisionsAllDevices": True,
        "ExpirationDate": datetime.datetime(2044, 1, 1),
        "DeveloperCertificates": [],
        "Name": "Syndeo Developer ID (stand-in)",
    }
    while rest:
        if rest[0] == "--certificate" and len(rest) >= 2:
            with open(rest[1], "rb") as f:
                profile["DeveloperCertificates"].append(f.read())
            rest = rest[2:]
            continue
        if rest[0] == "--app-id" and len(rest) >= 2:
            granted[APP_ID] = rest[1]
            rest = rest[2:]
            continue
        if rest[0] == "--without-passkeys":
            del granted[PASSKEYS]
        elif rest[0] == "--expired":
            profile["ExpirationDate"] = datetime.datetime(2020, 1, 1)
        elif rest[0] == "--devices":
            del profile["ProvisionsAllDevices"]
            profile["ProvisionedDevices"] = ["00008103-000A1B2C3D4E5F60"]
        else:
            return usage()
        rest = rest[1:]
    profile["Entitlements"] = granted
    openssl = shutil.which("openssl")
    if not openssl:
        sys.stderr.write("profile.py forge: needs openssl\n")
        return 1
    with tempfile.TemporaryDirectory() as d:
        key, crt, payload = (os.path.join(d, n) for n in ("key.pem", "crt.pem", "payload.plist"))
        subprocess.run([openssl, "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:prime256v1",
                        "-nodes", "-days", "1", "-subj", "/CN=Syndeo stand-in profile signer",
                        "-keyout", key, "-out", crt], capture_output=True, check=True)
        with open(payload, "wb") as f:
            f.write(plistlib.dumps(profile))
        subprocess.run([openssl, "cms", "-sign", "-nodetach", "-binary", "-outform", "DER", "-signer", crt,
                        "-inkey", key, "-in", payload, "-out", out], capture_output=True, check=True)
    return 0


def usage():
    sys.stderr.write(__doc__.split("\n\n")[1] + "\n")
    return 2


def self_test():
    cases = wrong = 0
    team = "AB12CD34EF"
    now = parse_now("2026-10-21T00:00:00Z")
    cert = b"\x30\x82certificate"

    def good(**changes):
        profile = {
            "Platform": ["OSX"],
            "TeamIdentifier": [team],
            "Entitlements": dict(entitlements(team)),
            "ProvisionsAllDevices": True,
            "ExpirationDate": datetime.datetime(2044, 1, 1),
            "DeveloperCertificates": [b"other", cert],
            "Name": "Syndeo Developer ID",
        }
        for key, value in changes.items():
            key = key.replace("__", ".")
            if value is None:
                profile.pop(key, None)
            else:
                profile[key] = value
        return profile

    def granted(**changes):
        e = dict(entitlements(team))
        for key, value in changes.items():
            key = {"app": APP_ID, "team": TEAM_KEY, "passkeys": PASSKEYS}.get(key, key)
            if value is None:
                e.pop(key, None)
            else:
                e[key] = value
        return e

    def case(label, profile, want, certificate=cert):
        nonlocal cases, wrong
        cases += 1
        payload = plistlib.dumps(profile) if isinstance(profile, dict) else profile
        got = problems(payload, team, certificate, now)
        verdict = (not got) if want == "" else any(want in p for p in got)
        if verdict:
            print(f"  ok    {label}")
        else:
            print(f"  WRONG {label}: {got}")
            wrong += 1

    print("\n  profile.py: Syndeo.app's Developer ID profile, exactly")
    case("a Developer ID profile for the App ID, authorizing passkeys, listing the certificate: accepted", good(), "")
    case("  ... without asking about a certificate: accepted", good(DeveloperCertificates=None), "", certificate=None)
    case("another team's profile: refused", good(TeamIdentifier=["ZZ12CD34EF"]), "its team is")
    case("two teams: refused", good(TeamIdentifier=[team, "ZZ12CD34EF"]), "its team is")
    case("another App ID: refused", good(Entitlements=granted(app=f"{team}.com.example.other")), "its App ID")
    case("a wildcard App ID: refused", good(Entitlements=granted(app=f"{team}.*")), "its App ID")
    case("another team's App ID: refused", good(Entitlements=granted(app=f"ZZ12CD34EF.{BUNDLE_ID}")), "its App ID")
    case("another team entitlement: refused", good(Entitlements=granted(team="ZZ12CD34EF")), "its team entitlement")
    case("without the passkey entitlement: refused", good(Entitlements=granted(passkeys=None)), "does not authorize")
    case("the passkey entitlement false: refused", good(Entitlements=granted(passkeys=False)), "does not authorize")
    case("the passkey entitlement the string 'true': refused", good(Entitlements=granted(passkeys="true")), "does not authorize")
    case("no entitlements at all: refused", good(Entitlements=None), "authorizes no entitlements")
    case("an iOS profile: refused", good(Platform=["iOS"]), "its platform")
    case("macOS and iOS: refused", good(Platform=["OSX", "iOS"]), "its platform")
    case("a development profile, tied to devices: refused",
         good(ProvisionsAllDevices=None, ProvisionedDevices=["00008103-000A"]), "not a Developer ID profile")
    case("for all devices and some devices too: refused", good(ProvisionedDevices=["00008103-000A"]), "not a Developer ID profile")
    case("expired: refused", good(ExpirationDate=datetime.datetime(2026, 10, 20)), "it expired")
    case("expiring this very second: refused", good(ExpirationDate=datetime.datetime(2026, 10, 21)), "it expired")
    case("no expiration date: refused", good(ExpirationDate=None), "no expiration date")
    case("not listing the signing certificate: refused", good(DeveloperCertificates=[b"other"]), "does not list the certificate")
    case("listing no certificates: refused", good(DeveloperCertificates=None), "does not list the certificate")
    case("not a property list: refused", b"garbage", "not a property list")
    case("a list, not a dictionary: refused", plistlib.dumps(["OSX"]), "not a dictionary")

    print("\n  profile.py: the entitlements Syndeo.app is signed with, exactly")
    with tempfile.TemporaryDirectory() as d:
        def signed_case(label, value, want):
            nonlocal cases, wrong
            cases += 1
            path = os.path.join(d, "e.xml")
            with open(path, "wb") as f:
                f.write(value if isinstance(value, bytes) else plistlib.dumps(value))
            with open(os.devnull, "w") as quiet:
                stdout, sys.stdout = sys.stdout, quiet
                try:
                    got = signed([path, team])
                finally:
                    sys.stdout = stdout
            if got == want:
                print(f"  ok    {label}")
            else:
                print(f"  WRONG {label}: exit {got}")
                wrong += 1

        signed_case("exactly the three: accepted", entitlements(team), 0)
        signed_case("none: refused", b"", 1)
        signed_case("the passkey entitlement alone: refused", {PASSKEYS: True}, 1)
        signed_case("one more, debugging allowed: refused",
                    dict(entitlements(team), **{"com.apple.security.get-task-allow": True}), 1)
        signed_case("one more, library validation off: refused",
                    dict(entitlements(team), **{"com.apple.security.cs.disable-library-validation": True}), 1)
        signed_case("another team's: refused", entitlements("ZZ12CD34EF"), 1)
        signed_case("not a property list: refused", b"garbage", 1)

    print("\n  profile.py: a profile read from its CMS signature")
    openssl = shutil.which("openssl")
    usable = openssl and subprocess.run([openssl, "cms", "-help"], capture_output=True).returncode == 0
    if not usable:
        print("  openssl with cms is not here: the CMS round trip is not run")
    else:
        with tempfile.TemporaryDirectory() as d:
            key, crt = os.path.join(d, "key.pem"), os.path.join(d, "crt.pem")
            payload_path, profile_path = os.path.join(d, "payload.plist"), os.path.join(d, "p.provisionprofile")
            subprocess.run([openssl, "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:prime256v1",
                            "-nodes", "-days", "1", "-subj", "/CN=Syndeo profile self-test",
                            "-keyout", key, "-out", crt], capture_output=True, check=True)
            profile = good()
            profile["ExpirationDate"] = datetime.datetime(2044, 1, 1)
            with open(payload_path, "wb") as f:
                f.write(plistlib.dumps(profile))
            subprocess.run([openssl, "cms", "-sign", "-nodetach", "-binary", "-outform", "DER", "-signer", crt,
                            "-inkey", key, "-in", payload_path, "-out", profile_path], capture_output=True, check=True)
            cases += 1
            decoded = decode(profile_path)
            if decoded is not None and plistlib.loads(decoded) == plistlib.loads(plistlib.dumps(profile)):
                print("  ok    a CMS-signed profile is read back exactly")
            else:
                print("  WRONG a CMS-signed profile is read back exactly")
                wrong += 1
            cases += 1
            if decode(payload_path) is None:
                print("  ok    a bare property list, not CMS-signed, is not a profile")
            else:
                print("  WRONG a bare property list, not CMS-signed, is not a profile")
                wrong += 1
            der = os.path.join(d, "cert.der")
            with open(der, "wb") as f:
                f.write(cert)
            for label, args, want in [
                ("check: the profile, the team and its certificate: exit 0", [profile_path, team, "--certificate", der], 0),
                ("check: another team: exit 1", [profile_path, "ZZ12CD34EF"], 1),
                ("check: a time after it expires: exit 1", [profile_path, team, "--now", "2044-01-02T00:00:00Z"], 1),
                ("check: not a profile: exit 1", [payload_path, team], 1),
                ("check: a team that is not one: exit 1", [profile_path, "ab12cd34ef"], 1),
            ]:
                cases += 1
                with open(os.devnull, "w") as quiet:
                    stdout, sys.stdout = sys.stdout, quiet
                    try:
                        got = check(args)
                    finally:
                        sys.stdout = stdout
                if got == want:
                    print(f"  ok    {label}")
                else:
                    print(f"  WRONG {label}: exit {got}")
                    wrong += 1

    print(f"\n  {cases} cases, {wrong} wrong\n")
    return 1 if wrong else 0


def main(argv):
    if argv == ["--self-test"]:
        return self_test()
    if len(argv) == 2 and argv[0] == "entitlements" and team_valid(argv[1]):
        sys.stdout.write(plistlib.dumps(entitlements(argv[1])).decode())
        return 0
    if argv and argv[0] == "check":
        return check(argv[1:])
    if argv and argv[0] == "signed":
        return signed(argv[1:])
    if argv and argv[0] == "forge":
        return forge(argv[1:])
    return usage()


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
