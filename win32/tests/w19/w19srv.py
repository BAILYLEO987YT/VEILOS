"""w19srv.py -- the host side of the W19 socket test, on 127.0.0.1 (VEILOS reaches
it as 10.0.2.2 through QEMU's user network).

TCP 47019: each line is a request. "ECHO text" answers "text"; "BIG n" answers n
bytes of a known pattern; "SLOW" answers after a second; "BYE" closes; "RESET"
drops the connection with a reset. When the
client shuts down its sending side, the server answers "SEEN END" and closes.
UDP 47019: each datagram comes back reversed.
Run it with python, leave it running, stop it with Ctrl+C."""
import socket, socketserver, struct, sys, threading, time

PORT = 47019


def pattern(n):
    return bytes((i * 7 + 3) & 0xFF for i in range(n))


class Tcp(socketserver.StreamRequestHandler):
    def handle(self):
        while True:
            line = self.rfile.readline()
            if not line:
                try:
                    self.wfile.write(b'SEEN END\n')
                except OSError:
                    pass
                return
            words = line.strip().split(b' ', 1)
            cmd = words[0]
            arg = words[1] if len(words) > 1 else b''
            if cmd == b'ECHO':
                self.wfile.write(arg + b'\n')
            elif cmd == b'BIG':
                self.wfile.write(pattern(int(arg)))
            elif cmd == b'SLOW':
                time.sleep(1)
                self.wfile.write(b'late\n')
            elif cmd == b'BYE':
                return
            elif cmd == b'RESET':
                # Linger on, for no time: closing sends RST, not FIN.
                self.connection.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack('HH', 1, 0))
                self.connection.close()
                return


class Udp(socketserver.BaseRequestHandler):
    def handle(self):
        data, sock = self.request
        sock.sendto(data[::-1], self.client_address)


class TcpServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def main():
    host = sys.argv[1] if len(sys.argv) > 1 else '127.0.0.1'
    tcp = TcpServer((host, PORT), Tcp)
    udp = socketserver.ThreadingUDPServer((host, PORT), Udp)
    for s in (tcp, udp):
        threading.Thread(target=s.serve_forever, daemon=True).start()
    print('w19srv on %s:%d' % (host, PORT), flush=True)
    try:
        while True:
            time.sleep(3600)
    except KeyboardInterrupt:
        pass


if __name__ == '__main__':
    main()
