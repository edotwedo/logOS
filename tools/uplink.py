#!/usr/bin/env python3
"""The logOS uplink bridge: a dumb wire until logOS has its own network driver.

Connects to the VM's serial port (QEMU listens on 127.0.0.1:4555), prints logOS's log, and
carries "@@ASK" messages to the hosted boss brain (Cloudflare Worker), sending back
"@@SAY", "@@DO" and "@@DONE". No keys, no state: the brain lives in the Worker.
"""
import json
import socket
import sys
import threading
import time
import urllib.request

UPLINK = "https://logos-uplink.phone-liaison.workers.dev/v1/think"
HOST, PORT = "127.0.0.1", int(sys.argv[1]) if len(sys.argv) > 1 else 4555


def connect():
    for _ in range(300):
        try:
            return socket.create_connection((HOST, PORT))
        except OSError:
            time.sleep(0.2)
    sys.exit("uplink: could not reach the VM's serial port")


def think(sock, lock, payload):
    try:
        req = urllib.request.Request(UPLINK, data=payload.encode(), headers={"Content-Type": "application/json", "User-Agent": "logOS-bridge/0.1"})
        with urllib.request.urlopen(req, timeout=60) as r:
            reply = json.load(r)
    except Exception as e:  # network trouble: tell logOS instead of hanging it
        reply = {"say": f"Uplink trouble: {str(e)[:80]}", "do": []}
    out = ["@@SAY " + " ".join(str(reply.get("say", "")).split())]
    for call in reply.get("do", []):
        out.append(f"@@DO {call.get('tool')} {call.get('value')}")
    out.append("@@DONE")
    print(f"  [boss: {reply.get('brain', '?')}] {reply.get('say', '')}  {reply.get('do', [])}", flush=True)
    with lock:
        for line in out:
            sock.sendall((line + "\n").encode("ascii", "replace"))


def main():
    sock = connect()
    lock = threading.Lock()
    print(f"uplink: wired to logOS on {HOST}:{PORT}, boss brain at {UPLINK}", flush=True)
    buf = b""
    while True:
        data = sock.recv(4096)
        if not data:
            print("uplink: logOS powered off", flush=True)
            return
        buf += data
        while b"\n" in buf:
            raw, buf = buf.split(b"\n", 1)
            line = raw.decode("utf-8", "replace").rstrip("\r")
            if line.startswith("@@READY"):
                with lock:
                    sock.sendall(b"@@HELLO GPT-OSS-120B (hosted)\n")
            elif line.startswith("@@ASK "):
                threading.Thread(target=think, args=(sock, lock, line[6:]), daemon=True).start()
            else:
                print(line, flush=True)


if __name__ == "__main__":
    main()
