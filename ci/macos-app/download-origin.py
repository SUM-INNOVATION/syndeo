#!/usr/bin/env python3
"""The origin ci/test-app.sh downloads and agent-downloads download from.

  download-origin.py <dir> <log> [<certificate chain> <key>]...

Serves the files in <dir>, and /redirect?t=<token>&to=<url>, a 302 to <url>,
over plain HTTP on 127.0.0.1 and, on the same port, on every IPv6 address
(lo0's link-local fe80::1 among them, with its scope); and, given
certificate chains and their keys, the same over TLS on 127.0.0.1. Prints
"http <port> https <port or none>" once it is listening.

Every connection serves one request and is closed (HTTP/1.0), and the TLS
server issues no session tickets, so every request a client makes comes on a
connection of its own, with a full handshake. Each TLS connection presents
the next of the certificates given, the first connection the first, so that
a client is shown a certificate it has not been shown before, and has to
decide anew whether to believe it, until they run out; then the last again.
Every response is no-store, so nothing can be served again from a cache
instead of asking it. A file whose name ends .bin is
application/octet-stream, which a browser downloads rather than shows.

Every request is logged to <log> as one JSON line: its path, the t= of its
query, which server took it (http or https), the connection it came on
(numbered as accepted, over both servers), the certificate its connection
presented (numbered from 1; 0 over http), and the address it came from; and
every TLS handshake, whether it succeeded, with the connection's number and
its certificate's.
"""
import http.server
import itertools
import json
import os
import socket
import socketserver
import ssl
import sys
import threading
import time
import urllib.parse

CONNECTIONS = itertools.count(1)
LOCK = threading.Lock()
# The connection the request on this thread came on, and its certificate.
CURRENT = threading.local()
# How many TLS connections have been taken.
SECURE = itertools.count(0)


def log(entry):
    entry["time"] = round(time.time(), 3)
    with LOCK:
        Handler.log.write(json.dumps(entry) + "\n")
        Handler.log.flush()


class Handler(http.server.BaseHTTPRequestHandler):
    log = None
    root = None
    # One request a connection, then closed: a client's next request comes
    # on a connection of its own.
    protocol_version = "HTTP/1.0"

    def log_message(self, *args):
        pass

    def do_GET(self):
        url = urllib.parse.urlsplit(self.path)
        query = dict(urllib.parse.parse_qsl(url.query))
        log({
            "path": url.path,
            "t": query.get("t", ""),
            "server": self.server.name,
            "conn": getattr(CURRENT, "conn", 0),
            "leaf": getattr(CURRENT, "leaf", 0),
            "from": self.client_address[0],
        })
        if url.path == "/redirect" and "to" in query:
            self.answer(302, b"", "text/plain", [("Location", query["to"])])
            return
        name = os.path.basename(url.path)
        path = os.path.join(Handler.root, name)
        if not name or name.startswith(".") or not os.path.isfile(path):
            self.answer(404, b"", "text/plain")
            return
        with open(path, "rb") as f:
            body = f.read()
        kind = "application/octet-stream" if name.endswith(".bin") else "text/html; charset=utf-8"
        self.answer(200, body, kind)

    def answer(self, status, body, kind, extra=()):
        self.send_response(status)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("Connection", "close")
        for name, value in extra:
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(body)


# TCPServer, not http.server's HTTPServer: that one looks this host's name up
# when it binds, which on the runner can take longer than the test waits.
class Server(socketserver.ThreadingMixIn, socketserver.TCPServer):
    daemon_threads = True
    allow_reuse_address = True
    name = "http"
    contexts = ()

    def finish_request(self, request, client_address):
        with LOCK:
            CURRENT.conn = next(CONNECTIONS)
            CURRENT.leaf = 0
            if self.contexts:
                CURRENT.leaf = min(next(SECURE), len(self.contexts) - 1) + 1
        if not self.contexts:
            super().finish_request(request, client_address)
            return
        # The handshake on the connection's own thread, so that one a client
        # abandons holds up no other.
        try:
            secure = self.contexts[CURRENT.leaf - 1].wrap_socket(request, server_side=True)
        except (ssl.SSLError, OSError) as err:
            log({"handshake": "failed", "server": self.name, "conn": CURRENT.conn,
                 "leaf": CURRENT.leaf, "error": err.__class__.__name__,
                 "reason": str(getattr(err, "reason", None) or err)[:120]})
            return
        log({"handshake": "ok", "server": self.name, "conn": CURRENT.conn, "leaf": CURRENT.leaf})
        try:
            super().finish_request(secure, client_address)
        finally:
            secure.close()

    def handle_error(self, request, client_address):
        # A client that goes away mid-answer is not this origin's failure.
        log({"error": str(sys.exc_info()[1])[:160], "server": self.name})


class Server6(Server):
    address_family = socket.AF_INET6

    def server_bind(self):
        # IPv6 only: the IPv4 server already holds this port for IPv4.
        self.socket.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
        super().server_bind()


def main(argv):
    if len(argv) < 2 or len(argv) % 2:
        sys.exit("usage: download-origin.py <dir> <log> [<certificate chain> <key>]...")
    Handler.root = argv[0]
    Handler.log = open(argv[1], "a", encoding="utf-8")
    four = Server(("127.0.0.1", 0), Handler)
    port = four.server_address[1]
    six = Server6(("::", port), Handler)
    servers = [four, six]
    tls_port = "none"
    if len(argv) > 2:
        contexts = []
        for chain, key in zip(argv[2::2], argv[3::2]):
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.load_cert_chain(chain, key)
            # No session to resume: every connection a full handshake.
            context.options |= ssl.OP_NO_TICKET
            context.num_tickets = 0
            contexts.append(context)
        secure = Server(("127.0.0.1", 0), Handler)
        secure.name = "https"
        secure.contexts = tuple(contexts)
        tls_port = secure.server_address[1]
        servers.append(secure)
    for server in servers[1:]:
        threading.Thread(target=server.serve_forever, daemon=True).start()
    print(f"http {port} https {tls_port}", flush=True)
    four.serve_forever()


if __name__ == "__main__":
    main(sys.argv[1:])
