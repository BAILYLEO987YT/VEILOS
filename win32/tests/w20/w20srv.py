"""w20srv.py -- the host side of the W20 TLS test: HTTPS on 127.0.0.1:47021
(VEILOS reaches it as 10.0.2.2), with the certificate in certs/, signed by the
test CA there. GET /hello answers a line; GET /big a megabyte of a known
pattern; anything else 404. Each connection is one request, closed cleanly
(close_notify). Run it with python, leave it running, stop it with Ctrl+C."""
import os, socket, ssl, sys, threading

PORT = 47021
HERE = os.path.dirname(os.path.abspath(__file__))


def pattern(n):
    return bytes((i * 13 + 5) & 0xFF for i in range(n))


BIG = pattern(1 << 20)


def serve(conn):
    try:
        request = b''
        while b'\r\n\r\n' not in request and len(request) < 8192:
            chunk = conn.recv(4096)
            if not chunk:
                return
            request += chunk
        path = request.split(b' ')[1] if request.count(b' ') >= 2 else b''
        if path == b'/hello':
            status, body = b'200 OK', b'hello over tls\n'
        elif path == b'/big':
            status, body = b'200 OK', BIG
        else:
            status, body = b'404 Not Found', b'no such page\n'
        conn.sendall(b'HTTP/1.1 ' + status + b'\r\nContent-Length: ' + str(len(body)).encode() +
                     b'\r\nConnection: close\r\n\r\n' + body)
        conn.unwrap()
    except (OSError, ssl.SSLError):
        pass
    finally:
        conn.close()


def main():
    host = sys.argv[1] if len(sys.argv) > 1 else '127.0.0.1'
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.minimum_version = ssl.TLSVersion.TLSv1_2
    ctx.load_cert_chain(os.path.join(HERE, 'certs', 'server.pem'), os.path.join(HERE, 'certs', 'server.key'))
    listener = socket.socket()
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind((host, PORT))
    listener.listen(16)
    print('w20srv on %s:%d' % (host, PORT), flush=True)
    while True:
        raw, _ = listener.accept()
        try:
            conn = ctx.wrap_socket(raw, server_side=True)
        except (OSError, ssl.SSLError):
            raw.close()
            continue
        threading.Thread(target=serve, args=(conn,), daemon=True).start()


if __name__ == '__main__':
    main()
