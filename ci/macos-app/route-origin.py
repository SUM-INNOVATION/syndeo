#!/usr/bin/env python3
"""The origin ci/test-app.sh routes measures against: which process each
request it is sent comes from, so that a page Syndeo.app's web view reached
directly can be told from one its proxy fetched.

  route-origin.py <log>

Listens on every IPv4 address and, on the same port, on every IPv6 address
(link-local ones included, with their scope), and prints "port <n> v6
<yes|no>" once it is listening. Every request is logged to <log>
as one JSON line: its path, the t= of its query, its Host, the address it
came from, and the process at the other end of its connection, found with
lsof while the connection is open: com.apple.WebKit.Networking for a request
WebKit made itself, syndeo-proxy for one the proxy made for it.

  /page?t=<token>[&sub=<url>]   a page saying ROUTE-PAGE-<token>, with an
                                image from <url> if there is one
  /img?t=<token>                a one-pixel GIF
  /redirect?t=<token>&to=<url>  302 to <url>
  /links?t=<token>&href=<url>   a page saying ROUTE-LINKS-<token>, with one
                                link, "go", to <url>
  /cached?t=<token>             a page saying ROUTE-CACHED-<token> that any
                                cache may keep for ten minutes
  /storage?t=<token>            a page that keeps <token> in its origin's
                                local storage unless something is there
                                already, then says ROUTE-STORAGE- and what is
                                there: "new:<token>" when it was empty

Every other response is no-store, so nothing it answers can be served again
from a cache instead of asking it.
"""
import html
import http.server
import json
import os
import socket
import socketserver
import subprocess
import sys
import time
import urllib.parse

ME = os.getpid()
GIF = bytes.fromhex(
    "47494638396101000100800000000000ffffff21f90401000000002c00000000010001000002024401003b"
)


def process_at(port):
    """The process, other than this one, with an open TCP connection on `port`."""
    try:
        out = subprocess.run(
            ["lsof", "+c", "0", "-nP", f"-iTCP:{port}", "-sTCP:ESTABLISHED", "-Fpc"],
            capture_output=True,
            text=True,
            timeout=10,
        ).stdout
    except (OSError, subprocess.SubprocessError) as err:
        return f"unknown ({err.__class__.__name__})"
    found, pid = set(), None
    for line in out.splitlines():
        if line.startswith("p"):
            pid = int(line[1:])
        elif line.startswith("c") and pid != ME:
            found.add(line[1:])
    return ",".join(sorted(found)) or "unknown"


class Handler(http.server.BaseHTTPRequestHandler):
    log = None

    def log_message(self, *args):
        pass

    def do_GET(self):
        url = urllib.parse.urlsplit(self.path)
        query = dict(urllib.parse.parse_qsl(url.query))
        entry = {
            "path": url.path,
            "t": query.get("t", ""),
            "host": self.headers.get("Host", ""),
            "from": self.client_address[0],
            "process": process_at(self.client_address[1]),
            "via": self.headers.get("Via", ""),
            "time": round(time.time(), 3),
        }
        Handler.log.write(json.dumps(entry) + "\n")
        Handler.log.flush()
        if url.path == "/redirect" and "to" in query:
            self.send_response(302)
            self.send_header("Location", query["to"])
            self.send_header("Cache-Control", "no-store")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        cache = "no-store"
        if url.path == "/img":
            body, kind = GIF, "image/gif"
        elif url.path == "/cached":
            token = html.escape(query.get("t", ""))
            body = (
                f"<!doctype html><meta charset=utf-8><title>Cached {token}</title>"
                f"<p>ROUTE-CACHED-{token}</p>"
            ).encode()
            kind = "text/html; charset=utf-8"
            cache = "public, max-age=600"
        elif url.path == "/storage":
            # Letters and digits only: it goes into a script.
            token = json.dumps("".join(c for c in query.get("t", "") if c.isascii() and c.isalnum()))
            body = (
                "<!doctype html><meta charset=utf-8><title>Storage</title><p id=o>...</p>"
                "<script>var v = localStorage.getItem('syndeo');"
                f"if (!v) {{ localStorage.setItem('syndeo', {token}); v = 'new:' + {token}; }}"
                "document.getElementById('o').textContent = 'ROUTE-STORAGE-' + v;</script>"
            ).encode()
            kind = "text/html; charset=utf-8"
        elif url.path == "/links":
            token = html.escape(query.get("t", ""))
            href = html.escape(query.get("href", ""))
            body = (
                f"<!doctype html><meta charset=utf-8><title>Links {token}</title>"
                f'<p>ROUTE-LINKS-{token}</p><p><a href="{href}">go</a></p>'
            ).encode()
            kind = "text/html; charset=utf-8"
        elif url.path == "/page":
            sub = query.get("sub", "")
            image = f'<img src="{html.escape(sub)}" width="1" height="1" alt="">' if sub else ""
            token = html.escape(query.get("t", ""))
            body = (
                f"<!doctype html><meta charset=utf-8><title>Route {token}</title>"
                f"<p>ROUTE-PAGE-{token}</p>{image}"
            ).encode()
            kind = "text/html; charset=utf-8"
        else:
            self.send_response(404)
            self.send_header("Cache-Control", "no-store")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        self.send_response(200)
        self.send_header("Content-Type", kind)
        self.send_header("Cache-Control", cache)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


# TCPServer, not http.server's HTTPServer: that one looks this host's name up
# when it binds, which on the runner can take longer than the test waits.
class Server(socketserver.ThreadingMixIn, socketserver.TCPServer):
    daemon_threads = True
    allow_reuse_address = True


class Server6(Server):
    address_family = socket.AF_INET6

    def server_bind(self):
        # IPv6 only: the IPv4 server already holds this port for IPv4.
        self.socket.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
        super().server_bind()


def main(argv):
    if len(argv) != 1:
        sys.exit("usage: route-origin.py <log>")
    Handler.log = open(argv[0], "a", encoding="utf-8")
    four = Server(("0.0.0.0", 0), Handler)
    port = four.server_address[1]
    try:
        six = Server6(("::", port), Handler)
    except OSError:
        six = None
    if six is not None:
        import threading

        threading.Thread(target=six.serve_forever, daemon=True).start()
    print(f"port {port} v6 {'yes' if six else 'no'}", flush=True)
    four.serve_forever()


if __name__ == "__main__":
    main(sys.argv[1:])
