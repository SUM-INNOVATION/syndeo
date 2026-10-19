#!/usr/bin/env python3
"""The origin ci/test-app.sh realtime measures against: what WebSockets and
WebRTC do in Syndeo.app, recorded and not judged.

  realtime-origin.py <log>

Listens on every IPv4 address, for TCP and for UDP on the same port, and
prints "port <n>" once it is listening. Every request, every WebSocket
handshake and every datagram is logged to <log> as one JSON line, with the
process at the other end, found with lsof while its connection or socket is
open: com.apple.WebKit.Networking for WebKit itself, syndeo-proxy for the
proxy.

  /ws?t=<token>&to=<where>   a page that opens a WebSocket, sends
                             "ping-<token>", and says what happened, in
                             order, after RT-WS-<token>: open, each message
                             (echo, for its own ping), error, and the close's
                             code and reason; then RT-DONE-<token>. <where>:
                               loop  ws://127.0.0.1:<this port>/echo
                               name  ws://127.0.0.1.nip.io:<this port>/echo
                               echo  ws://echo.websocket.org/
                               echos wss://echo.websocket.org/
  /echo?t=<token>            a WebSocket endpoint that echoes text. Logged
                             with the Upgrade, Connection and
                             Sec-WebSocket-Key headers it was sent; answered
                             101 only when they make a handshake, and 400
                             otherwise.
  /rtc?t=<token>&lan=<addr>  a page that tries WebRTC and says, after
                             RT-RTC-<token>: whether RTCPeerConnection exists;
                             the ICE candidates a data channel's offer
                             gathers, by type, with STUN at <addr> and
                             127.0.0.1 on this port and at
                             stun.l.google.com:19302; and whether two
                             connections in the page connect and pass a
                             message, and by which candidates; then
                             RT-DONE-<token>.
  UDP, the same port         a STUN server: a Binding request is answered
                             with the address and port it came from.

Every response is no-store.
"""
import base64
import hashlib
import html
import http.server
import json
import os
import socket
import socketserver
import struct
import subprocess
import sys
import threading
import time
import urllib.parse

ME = os.getpid()
GUID = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
COOKIE = 0x2112A442
LOG_LOCK = threading.Lock()
LOG = None


def log(entry):
    entry["time"] = round(time.time(), 3)
    with LOG_LOCK:
        LOG.write(json.dumps(entry) + "\n")
        LOG.flush()


def process_at(proto, port):
    """The processes, other than this one, with a socket on `port`."""
    args = ["lsof", "+c", "0", "-nP", f"-i{proto}:{port}", "-Fpc"]
    if proto == "TCP":
        args[-1:-1] = ["-sTCP:ESTABLISHED"]
    try:
        out = subprocess.run(args, capture_output=True, text=True, timeout=10).stdout
    except (OSError, subprocess.SubprocessError) as err:
        return f"unknown ({err.__class__.__name__})"
    found, pid = set(), None
    for line in out.splitlines():
        if line.startswith("p"):
            pid = int(line[1:])
        elif line.startswith("c") and pid != ME:
            found.add(line[1:])
    return ",".join(sorted(found)) or "unknown"


def token_of(query):
    """The token, letters and digits only: it goes into a page and a script."""
    return "".join(c for c in query.get("t", "") if c.isascii() and c.isalnum())


WS_PAGE = """<!doctype html><meta charset=utf-8><title>WebSocket {t}</title>
<p id=s>RT-WS-{t} starting</p><p id=d></p>
<script>
var t = {tj}, to = {toj}, said = [];
function say(x) {{ said.push(x); document.getElementById('s').textContent = 'RT-WS-' + t + ' ' + said.join('; '); }}
function done() {{ document.getElementById('d').textContent = 'RT-DONE-' + t; }}
say('to ' + to);
var timer = setTimeout(function () {{ say('no close within 15 s'); done(); }}, 15000);
try {{
  var ws = new WebSocket(to);
  ws.onopen = function () {{ say('open'); ws.send('ping-' + t); }};
  ws.onmessage = function (e) {{
    var m = String(e.data);
    say('message ' + (m === 'ping-' + t ? 'echo' : JSON.stringify(m.slice(0, 60))));
    if (m === 'ping-' + t) ws.close(1000, 'done');
  }};
  ws.onerror = function () {{ say('error'); }};
  ws.onclose = function (e) {{
    say('close ' + e.code + (e.reason ? ' ' + e.reason : '') + (e.wasClean ? ' clean' : ' not clean'));
    clearTimeout(timer); done();
  }};
}} catch (e) {{ say('threw ' + e.name + ': ' + e.message); clearTimeout(timer); done(); }}
</script>"""

RTC_PAGE = """<!doctype html><meta charset=utf-8><title>WebRTC {t}</title>
<p id=s>RT-RTC-{t} starting</p><p id=c></p><p id=p></p><p id=d></p>
<script>
var t = {tj}, stuns = {stunsj};
function line(id, x) {{ document.getElementById(id).textContent = 'RT-RTC-' + t + ' ' + x; }}
function done() {{ document.getElementById('d').textContent = 'RT-DONE-' + t; }}
(async function () {{
  if (typeof RTCPeerConnection !== 'function') {{ line('s', 'RTCPeerConnection: ' + typeof RTCPeerConnection); done(); return; }}
  line('s', 'RTCPeerConnection: a function');
  try {{
    var counts = {{}}, srflx = [], gathered = 'gathering not complete in 10 s';
    var pc = new RTCPeerConnection({{iceServers: stuns.map(function (s) {{ return {{urls: 'stun:' + s}}; }})}});
    pc.createDataChannel('probe');
    await new Promise(function (resolve, reject) {{
      var timer = setTimeout(resolve, 10000);
      pc.onicecandidate = function (e) {{
        if (!e.candidate) {{ gathered = 'gathering complete'; clearTimeout(timer); resolve(); return; }}
        var c = e.candidate.candidate, m = / typ (\\w+)/.exec(c), ty = m ? m[1] : '?';
        counts[ty] = (counts[ty] || 0) + 1;
        if (ty === 'srflx') {{ var a = c.split(' '); srflx.push(a[4] + ':' + a[5] + '/' + a[2]); }}
      }};
      pc.createOffer().then(function (o) {{ return pc.setLocalDescription(o); }}).catch(reject);
    }});
    line('c', 'candidates: ' + (Object.keys(counts).sort().map(function (k) {{ return k + ' ' + counts[k]; }}).join(', ') || 'none') +
      '; ' + gathered + (srflx.length ? '; srflx ' + srflx.join(' ') : ''));
    pc.close();
  }} catch (e) {{ line('c', 'candidates: threw ' + e.name + ': ' + e.message); }}
  try {{
    var a = new RTCPeerConnection(), b = new RTCPeerConnection();
    a.onicecandidate = function (e) {{ if (e.candidate) b.addIceCandidate(e.candidate); }};
    b.onicecandidate = function (e) {{ if (e.candidate) a.addIceCandidate(e.candidate); }};
    var got = new Promise(function (resolve) {{
      var timer = setTimeout(function () {{ resolve('no message in 10 s'); }}, 10000);
      b.ondatachannel = function (e) {{ e.channel.onmessage = function (m) {{ clearTimeout(timer); resolve('a message passed: ' + m.data); }}; }};
    }});
    var ch = a.createDataChannel('pair');
    ch.onopen = function () {{ ch.send('hello-' + t); }};
    await a.setLocalDescription(await a.createOffer());
    await b.setRemoteDescription(a.localDescription);
    await b.setLocalDescription(await b.createAnswer());
    await a.setRemoteDescription(b.localDescription);
    var said = await got, pair = null, cands = {{}};
    (await a.getStats()).forEach(function (r) {{
      if (r.type === 'candidate-pair' && r.state === 'succeeded' && !pair) pair = r;
      if (r.type === 'local-candidate' || r.type === 'remote-candidate') cands[r.id] = r;
    }});
    if (pair) {{
      var l = cands[pair.localCandidateId] || {{}}, r = cands[pair.remoteCandidateId] || {{}};
      said += '; by ' + (l.candidateType || '?') + ' ' + (l.protocol || '?') + ' to ' + (r.candidateType || '?') + ' ' + (r.protocol || '?');
    }}
    line('p', 'two connections in the page: ' + said + '; ice ' + a.iceConnectionState);
    a.close(); b.close();
  }} catch (e) {{ line('p', 'two connections in the page: threw ' + e.name + ': ' + e.message); }}
  done();
}})();
</script>"""


class Handler(http.server.BaseHTTPRequestHandler):
    # A WebSocket handshake is answered in HTTP/1.1.
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def entry(self, url, query):
        return {
            "path": url.path,
            "t": token_of(query),
            "host": self.headers.get("Host", ""),
            "from": self.client_address[0],
            "process": process_at("TCP", self.client_address[1]),
            "via": self.headers.get("Via", ""),
        }

    def page(self, body):
        body = body.encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        url = urllib.parse.urlsplit(self.path)
        query = dict(urllib.parse.parse_qsl(url.query))
        entry = self.entry(url, query)
        t = entry["t"]
        port = self.server.server_address[1]
        if url.path == "/echo":
            return self.echo(entry)
        log(entry)
        if url.path == "/ws":
            to = {
                "loop": f"ws://127.0.0.1:{port}/echo?t={t}",
                "name": f"ws://127.0.0.1.nip.io:{port}/echo?t={t}",
                "echo": "ws://echo.websocket.org/",
                "echos": "wss://echo.websocket.org/",
            }.get(query.get("to", ""), "")
            return self.page(WS_PAGE.format(t=html.escape(t), tj=json.dumps(t), toj=json.dumps(to)))
        if url.path == "/rtc":
            lan = query.get("lan", "")
            try:
                socket.inet_aton(lan)
            except OSError:
                lan = ""
            stuns = ([f"{lan}:{port}"] if lan else []) + [f"127.0.0.1:{port}", "stun.l.google.com:19302"]
            return self.page(RTC_PAGE.format(t=html.escape(t), tj=json.dumps(t), stunsj=json.dumps(stuns)))
        self.send_response(404)
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", "0")
        self.end_headers()

    def echo(self, entry):
        upgrade = self.headers.get("Upgrade", "")
        connection = self.headers.get("Connection", "")
        key = self.headers.get("Sec-WebSocket-Key", "")
        handshake = upgrade.lower() == "websocket" and "upgrade" in connection.lower() and bool(key)
        entry.update(upgrade=upgrade, connection=connection, key="yes" if key else "no",
                     answered=101 if handshake else 400)
        log(entry)
        if not handshake:
            body = b"not a WebSocket handshake"
            self.send_response(400)
            self.send_header("Cache-Control", "no-store")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        accept = base64.b64encode(hashlib.sha1(key.encode() + GUID).digest()).decode()
        self.send_response(101, "Switching Protocols")
        self.send_header("Upgrade", "websocket")
        self.send_header("Connection", "Upgrade")
        self.send_header("Sec-WebSocket-Accept", accept)
        self.end_headers()
        self.wfile.flush()
        self.close_connection = True
        echoed = 0
        while True:
            frame = self.frame()
            if frame is None:
                break
            opcode, payload = frame
            if opcode == 0x1:
                self.send_frame(0x1, payload)
                echoed += 1
            elif opcode == 0x9:
                self.send_frame(0xA, payload)
            elif opcode == 0x8:
                self.send_frame(0x8, payload[:2])
                break
        log({"path": "/echo", "t": entry["t"], "echoed": echoed})

    def frame(self):
        """One frame a client sent, unmasked: (opcode, payload), or None at its end."""
        head = self.rfile.read(2)
        if len(head) < 2:
            return None
        opcode, length = head[0] & 0x0F, head[1] & 0x7F
        if length == 126:
            length = struct.unpack("!H", self.rfile.read(2))[0]
        elif length == 127:
            length = struct.unpack("!Q", self.rfile.read(8))[0]
        mask = self.rfile.read(4) if head[1] & 0x80 else b"\0\0\0\0"
        payload = self.rfile.read(length)
        return opcode, bytes(b ^ mask[i % 4] for i, b in enumerate(payload))

    def send_frame(self, opcode, payload):
        n = len(payload)
        head = bytes([0x80 | opcode])
        if n < 126:
            head += bytes([n])
        elif n < 65536:
            head += bytes([126]) + struct.pack("!H", n)
        else:
            head += bytes([127]) + struct.pack("!Q", n)
        self.wfile.write(head + payload)
        self.wfile.flush()


def stun_answer(data, address):
    """The Binding success response to a STUN Binding request from `address`."""
    if len(data) < 20:
        return None
    kind, _, cookie = struct.unpack("!HHI", data[:8])
    if kind != 0x0001 or cookie != COOKIE:
        return None
    port = address[1] ^ (COOKIE >> 16)
    ip = bytes(a ^ b for a, b in zip(socket.inet_aton(address[0]), struct.pack("!I", COOKIE)))
    attribute = struct.pack("!HHBBH", 0x0020, 8, 0, 1, port) + ip
    return struct.pack("!HHI", 0x0101, len(attribute), COOKIE) + data[8:20] + attribute


def stun(sock):
    while True:
        data, address = sock.recvfrom(2048)
        answer = stun_answer(data, address)
        log({"udp": True, "from": address[0], "port": address[1],
             "stun": "binding" if answer else "other", "bytes": len(data),
             "process": process_at("UDP", address[1])})
        if answer:
            sock.sendto(answer, address)


# TCPServer, not http.server's HTTPServer: that one looks this host's name up
# when it binds, which on the runner can take longer than the test waits.
class Server(socketserver.ThreadingMixIn, socketserver.TCPServer):
    daemon_threads = True
    allow_reuse_address = True


def main(argv):
    global LOG
    if len(argv) != 1:
        sys.exit("usage: realtime-origin.py <log>")
    LOG = open(argv[0], "a", encoding="utf-8")
    tcp = Server(("0.0.0.0", 0), Handler)
    port = tcp.server_address[1]
    udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    udp.bind(("0.0.0.0", port))
    threading.Thread(target=stun, args=(udp,), daemon=True).start()
    print(f"port {port}", flush=True)
    tcp.serve_forever()


if __name__ == "__main__":
    main(sys.argv[1:])
