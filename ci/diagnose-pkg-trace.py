"""Diagnostic only: trace what an install does to Syndeo's package, in order.

    python3 -I ci/diagnose-pkg-trace.py STOP [ROOT]

Until the file STOP exists, it looks every millisecond at:
- ROOT/usr/local/libexec/syndeo: each entry, how many entries each version
  directory holds, and where `current` and `.current.new` point;
- the receipt, ROOT/var/db/receipts/com.sum.syndeo.pkg.plist: its version and
  install prefix;
- ROOT/usr/local/bin/syndeo: absent, dangling, or resolving.
It prints a line, with the UTC time to the millisecond, each time any of them
changes. ROOT is empty on a runner and a scratch directory when trying it out.
"""
import datetime
import os
import plistlib
import sys
import time


def main():
    stop = sys.argv[1]
    root = sys.argv[2] if len(sys.argv) > 2 else ""
    private = root + "/usr/local/libexec/syndeo"
    receipt = root + "/var/db/receipts/com.sum.syndeo.pkg.plist"
    link = root + "/usr/local/bin/syndeo"

    def entries():
        try:
            names = sorted(os.listdir(private))
        except FileNotFoundError:
            return "absent"
        except OSError as e:
            return "unreadable (%s)" % e.strerror
        out = []
        for name in names:
            path = os.path.join(private, name)
            try:
                if os.path.islink(path):
                    out.append("%s->%s" % (name, os.readlink(path)))
                elif os.path.isdir(path):
                    out.append("%s/[%d]" % (name, len(os.listdir(path))))
                else:
                    out.append(name)
            except OSError:
                out.append("%s(vanishing)" % name)
        return " ".join(out) or "empty"

    def receipt_state():
        try:
            with open(receipt, "rb") as f:
                data = f.read()
        except FileNotFoundError:
            return "none"
        except OSError as e:
            return "unreadable (%s)" % e.strerror
        try:
            p = plistlib.loads(data)
        except Exception:
            return "partial (%d bytes)" % len(data)
        return "%s at '%s'" % (p.get("PackageVersion"), p.get("InstallPrefixPath"))

    def link_state():
        if not os.path.lexists(link):
            return "absent"
        return "resolves" if os.path.exists(link) else "dangling"

    started = time.monotonic()
    last = None
    while not os.path.exists(stop):
        now = (entries(), receipt_state(), link_state())
        if now != last:
            stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%H:%M:%S.%f")[:-3]
            print("%s +%8.1f ms  private: %s | receipt: %s | bin/syndeo: %s"
                  % (stamp, (time.monotonic() - started) * 1000, now[0], now[1], now[2]), flush=True)
            last = now
        time.sleep(0.001)
    return 0


if __name__ == "__main__":
    sys.exit(main())
