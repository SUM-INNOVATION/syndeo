#!/usr/bin/env python3
"""The agent's side of ci/test-app.sh agents: what a program asking to drive
Syndeo does, and reading what it was answered.

  agent-driver.py field FILE PATH      the value at PATH (a.b.0.c) in the JSON
                                       answer on FILE's last line; strings bare,
                                       anything else as JSON; exits 1 if absent
  agent-driver.py ref FILE ROLE NAME   in a snapshot's answer in FILE, the ref
                                       of the first element of ROLE whose name
                                       contains NAME; exits 1 if there is none
  agent-driver.py png FILE             the screenshot answer in FILE holds a PNG
                                       of the width and height it says: prints
                                       "WIDTHxHEIGHT, N bytes"; exits 1 if not
  agent-driver.py raw SOCKET NAME      connect to SOCKET itself, say hello as
                                       NAME, and print the first answer line
                                       within 15 s, or "no answer"
  agent-driver.py as-agent CMD...      run CMD as this process's child, so that
                                       this process is the agent the person is
                                       asked about; exits with CMD's status
  agent-driver.py mcp BIN URL OUT      an MCP session with `BIN mcp`, as
                                       "ci-mcp 1.0": initialize, tools/list,
                                       syndeo_open URL, syndeo_read; what each
                                       answered, as JSON, in OUT
"""
import base64
import json
import select
import socket
import struct
import subprocess
import sys
import time


def last_answer(path):
    with open(path, encoding="utf-8") as f:
        lines = [line for line in f.read().splitlines() if line.strip()]
    if not lines:
        sys.exit("no answer in %s" % path)
    return json.loads(lines[-1])


def at(value, path):
    for part in path.split("."):
        if isinstance(value, list) and part.isdigit() and int(part) < len(value):
            value = value[int(part)]
        elif isinstance(value, dict) and part in value:
            value = value[part]
        else:
            sys.exit(1)
    return value


def field(path, where):
    value = at(last_answer(path), where)
    print(value if isinstance(value, str) else json.dumps(value))


def ref(path, role, name):
    for element in at(last_answer(path), "result.elements"):
        if element.get("role") == role and name in element.get("name", ""):
            print(element["ref"])
            return
    sys.exit(1)


def png(path):
    result = at(last_answer(path), "result")
    data = base64.b64decode(result["png"], validate=True)
    if data[:8] != b"\x89PNG\r\n\x1a\n" or data[12:16] != b"IHDR":
        sys.exit("not a PNG")
    width, height = struct.unpack(">II", data[16:24])
    if (width, height) != (result["width"], result["height"]) or not width or not height:
        sys.exit("a PNG of %dx%d, said to be %sx%s" % (width, height, result["width"], result["height"]))
    print("%dx%d, %d bytes" % (width, height, len(data)))


def raw(path, name):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
        s.connect(path)
        s.sendall((json.dumps({"cmd": "hello", "client": name}) + "\n").encode())
        s.settimeout(15)
        got = b""
        try:
            while b"\n" not in got:
                chunk = s.recv(65536)
                if not chunk:
                    break
                got += chunk
        except socket.timeout:
            pass
    line = got.split(b"\n")[0].decode(errors="replace").strip()
    print(line or "no answer")


def mcp(binary, url, out):
    child = subprocess.Popen([binary, "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE)
    record = {}
    deadline = time.monotonic() + 240

    def send(message):
        child.stdin.write((json.dumps(message) + "\n").encode())
        child.stdin.flush()

    def answer(want):
        while time.monotonic() < deadline:
            ready, _, _ = select.select([child.stdout], [], [], 1)
            if not ready:
                continue
            line = child.stdout.readline()
            if not line:
                raise RuntimeError("the server closed its stdout")
            message = json.loads(line)
            if message.get("id") == want:
                return message
        raise RuntimeError("no answer to %s in time" % want)

    try:
        send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "ci-mcp", "version": "1.0"}}})
        record["initialize"] = answer(1)
        send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        send({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
        record["tools"] = [t["name"] for t in answer(2)["result"]["tools"]]
        send({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
              "params": {"name": "syndeo_open", "arguments": {"url": url}}})
        opened = answer(3)
        record["open"] = opened
        text = opened["result"]["content"][0]["text"]
        tab = json.loads(text)["tab"] if not opened["result"].get("isError") else None
        if tab is not None:
            send({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
                  "params": {"name": "syndeo_read", "arguments": {"tab": tab}}})
            read = answer(4)
            record["read"] = json.loads(read["result"]["content"][0]["text"])
    except Exception as e:  # recorded, and judged by the test
        record["error"] = str(e)
    finally:
        child.stdin.close()
        try:
            record["exit"] = child.wait(timeout=15)
        except subprocess.TimeoutExpired:
            child.kill()
            record["exit"] = "killed"
        record["stderr"] = child.stderr.read().decode(errors="replace")[-2000:]
    with open(out, "w", encoding="utf-8") as f:
        json.dump(record, f)
        f.write("\n")


def main(argv):
    if len(argv) == 3 and argv[0] == "field":
        return field(argv[1], argv[2])
    if len(argv) == 4 and argv[0] == "ref":
        return ref(argv[1], argv[2], argv[3])
    if len(argv) == 2 and argv[0] == "png":
        return png(argv[1])
    if len(argv) == 3 and argv[0] == "raw":
        return raw(argv[1], argv[2])
    if len(argv) >= 2 and argv[0] == "as-agent":
        sys.exit(subprocess.call(argv[1:]))
    if len(argv) == 4 and argv[0] == "mcp":
        return mcp(argv[1], argv[2], argv[3])
    sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv[1:])
