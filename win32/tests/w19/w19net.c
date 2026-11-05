/* W19 (S4): Winsock. Start-up, names, a TCP conversation with w19srv.py (on
 * Windows at 127.0.0.1, from VEILOS at 10.0.2.2: the first argument), a refused
 * connection, non-blocking sockets and select, a listener on the loopback
 * address with this program as its own client, and UDP.
 * Run with: w19net.exe SERVER-ADDRESS */
#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>
#include <stdio.h>
#include <string.h>

#define PORT 47019
#define CLOSED_PORT 47020

static const char *server = "127.0.0.1";

static unsigned checksum(const unsigned char *p, int n)
{
    unsigned h = 2166136261u;
    for (int i = 0; i < n; i++)
        h = (h ^ p[i]) * 16777619u;
    return h;
}

static struct sockaddr_in at(const char *ip, int port)
{
    struct sockaddr_in a;
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_port = htons((u_short)port);
    inet_pton(AF_INET, ip, &a.sin_addr);
    return a;
}

/* Everything up to and including the next newline. */
static int read_line(SOCKET s, char *buf, int size)
{
    int n = 0;
    while (n < size - 1) {
        int r = recv(s, buf + n, 1, 0);
        if (r <= 0)
            break;
        if (buf[n++] == '\n')
            break;
    }
    buf[n] = 0;
    return n;
}

static int recv_all(SOCKET s, char *buf, int want)
{
    int got = 0;
    while (got < want) {
        int r = recv(s, buf + got, want - got, 0);
        if (r <= 0)
            break;
        got += r;
    }
    return got;
}

static void startup(void)
{
    printf("--- start-up\n");
    WSADATA wsa;
    int r = WSAStartup(MAKEWORD(2, 2), &wsa);
    printf("WSAStartup: %d version %d.%d\n", r, LOBYTE(wsa.wVersion), HIBYTE(wsa.wVersion));
    int bad = closesocket((SOCKET)12345);
    int bad_error = WSAGetLastError();
    printf("bad socket: %d error %d\n", bad, bad_error);
    SOCKET s = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    printf("tcp socket: %d\n", s != INVALID_SOCKET);
    printf("closed: %d\n", closesocket(s));
    s = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
    printf("udp socket: %d\n", s != INVALID_SOCKET);
    closesocket(s);
    s = socket(12345, SOCK_STREAM, 0);
    int e = WSAGetLastError();
    printf("unknown family: %d error %d\n", s != INVALID_SOCKET, e);
}

static void names(void)
{
    printf("--- names\n");
    struct addrinfo hints, *res = NULL;
    memset(&hints, 0, sizeof hints);
    hints.ai_family = AF_INET;
    hints.ai_socktype = SOCK_STREAM;
    int r = getaddrinfo("127.0.0.1", "80", &hints, &res);
    char text[64] = "";
    if (r == 0)
        inet_ntop(AF_INET, &((struct sockaddr_in *)res->ai_addr)->sin_addr, text, sizeof text);
    printf("numeric: %d %s port %d\n", r, text, r == 0 ? ntohs(((struct sockaddr_in *)res->ai_addr)->sin_port) : 0);
    if (res)
        freeaddrinfo(res);
    res = NULL;
    r = getaddrinfo("localhost", NULL, &hints, &res);
    text[0] = 0;
    if (r == 0)
        inet_ntop(AF_INET, &((struct sockaddr_in *)res->ai_addr)->sin_addr, text, sizeof text);
    printf("localhost: %d %s\n", r, text);
    if (res)
        freeaddrinfo(res);
    res = NULL;
    r = getaddrinfo("example.com", "443", &hints, &res);
    printf("example.com: %d, has an address: %d\n", r, r == 0 && res && res->ai_addr != NULL);
    if (res)
        freeaddrinfo(res);
    res = NULL;
    r = getaddrinfo("no-such-host.invalid", NULL, &hints, &res);
    printf("no such host: %d\n", r);
    struct in_addr a;
    printf("inet_pton: %d\n", inet_pton(AF_INET, "10.1.2.3", &a));
    printf("inet_ntop: %s\n", inet_ntop(AF_INET, &a, text, sizeof text));
    printf("bad text: %d\n", inet_pton(AF_INET, "10.1.2", &a));
    char host[256] = "";
    int named = gethostname(host, sizeof host);
    printf("gethostname: %d, has a name: %d\n", named, host[0] != 0);
}

static void conversation(void)
{
    printf("--- a conversation\n");
    SOCKET s = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    struct sockaddr_in a = at(server, PORT);
    printf("connect: %d\n", connect(s, (struct sockaddr *)&a, sizeof a));
    struct sockaddr_in peer;
    int len = sizeof peer;
    getpeername(s, (struct sockaddr *)&peer, &len);
    printf("peer port: %d, length %d\n", ntohs(peer.sin_port), len);
    struct sockaddr_in me;
    len = sizeof me;
    getsockname(s, (struct sockaddr *)&me, &len);
    printf("own port given: %d\n", ntohs(me.sin_port) != 0);
    BOOL on = TRUE;
    printf("nodelay: %d\n", setsockopt(s, IPPROTO_TCP, TCP_NODELAY, (char *)&on, sizeof on));
    char line[256];
    const char *hello = "ECHO hello there\n";
    printf("sent: %d\n", send(s, hello, (int)strlen(hello), 0));
    read_line(s, line, sizeof line);
    printf("echo: %s", line);
    const char *big = "BIG 1000000\n";
    send(s, big, (int)strlen(big), 0);
    static char data[1000000];
    int got = recv_all(s, data, sizeof data);
    printf("big: %d bytes, checksum %08x\n", got, checksum((unsigned char *)data, got));
    send(s, "ECHO after\n", 11, 0);
    read_line(s, line, sizeof line);
    printf("then: %s", line);
    printf("shutdown: %d\n", shutdown(s, SD_SEND));
    read_line(s, line, sizeof line);
    printf("server saw the end: %s", line);
    printf("then the end: %d\n", recv(s, line, sizeof line, 0));
    closesocket(s);
}

static void refused(void)
{
    printf("--- refused\n");
    SOCKET s = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    struct sockaddr_in a = at("127.0.0.1", CLOSED_PORT);
    int r = connect(s, (struct sockaddr *)&a, sizeof a);
    int e = WSAGetLastError();
    printf("connect: %d error %d\n", r, e);
    closesocket(s);
}

static void nonblocking(void)
{
    printf("--- non-blocking\n");
    SOCKET s = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    u_long on = 1;
    printf("ioctlsocket: %d\n", ioctlsocket(s, FIONBIO, &on));
    struct sockaddr_in a = at(server, PORT);
    int r = connect(s, (struct sockaddr *)&a, sizeof a);
    int e = WSAGetLastError();
    printf("connect: %d error %d\n", r, e);
    fd_set w, x;
    FD_ZERO(&w);
    FD_SET(s, &w);
    FD_ZERO(&x);
    FD_SET(s, &x);
    struct timeval tv = {5, 0};
    r = select(0, NULL, &w, &x, &tv);
    printf("select: %d, writable %d, failed %d\n", r, FD_ISSET(s, &w) != 0, FD_ISSET(s, &x) != 0);
    char buf[64];
    r = recv(s, buf, sizeof buf, 0);
    e = WSAGetLastError();
    printf("nothing yet: %d error %d\n", r, e);
    send(s, "SLOW\n", 5, 0);
    fd_set rd;
    FD_ZERO(&rd);
    FD_SET(s, &rd);
    struct timeval brief = {0, 100000};
    printf("not readable soon: %d\n", select(0, &rd, NULL, NULL, &brief));
    FD_ZERO(&rd);
    FD_SET(s, &rd);
    struct timeval wait = {5, 0};
    printf("readable later: %d %d\n", select(0, &rd, NULL, NULL, &wait), FD_ISSET(s, &rd) != 0);
    r = recv(s, buf, sizeof buf - 1, 0);
    buf[r > 0 ? r : 0] = 0;
    printf("got: %d %s", r, buf);
    WSAPOLLFD p = {s, POLLRDNORM, 0};
    printf("poll, nothing: %d revents %d\n", WSAPoll(&p, 1, 50), p.revents);
    closesocket(s);
}

/* A server that drops the connection: a reset, which a refused connection
   would also be if QEMU's network (on a Windows host) passed refusals on. */
static void reset(void)
{
    printf("--- reset\n");
    SOCKET s = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    struct sockaddr_in a = at(server, PORT);
    printf("connect: %d\n", connect(s, (struct sockaddr *)&a, sizeof a));
    send(s, "RESET\n", 6, 0);
    char buf[16];
    int r = recv(s, buf, sizeof buf, 0);
    int e = WSAGetLastError();
    printf("recv: %d error %d\n", r, e);
    r = send(s, "more\n", 5, 0);
    e = WSAGetLastError();
    printf("send after: %d error %d\n", r, e);
    closesocket(s);
}

/* This program as its own server, on the loopback address. */
static void loopback(void)
{
    printf("--- a listener\n");
    SOCKET l = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    struct sockaddr_in a = at("127.0.0.1", 0);
    printf("bind: %d\n", bind(l, (struct sockaddr *)&a, sizeof a));
    int len = sizeof a;
    getsockname(l, (struct sockaddr *)&a, &len);
    printf("port given: %d\n", ntohs(a.sin_port) != 0);
    printf("listen: %d\n", listen(l, 4));
    SOCKET c[2];
    for (int i = 0; i < 2; i++) {
        c[i] = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
        printf("client %d connect: %d\n", i, connect(c[i], (struct sockaddr *)&a, sizeof a));
    }
    for (int i = 0; i < 2; i++) {
        struct sockaddr_in from;
        int flen = sizeof from;
        SOCKET s = accept(l, (struct sockaddr *)&from, &flen);
        char ip[32];
        inet_ntop(AF_INET, &from.sin_addr, ip, sizeof ip);
        printf("accepted %d: %d from %s\n", i, s != INVALID_SOCKET, ip);
        char msg[32];
        sprintf(msg, "to client %d\n", i);
        send(s, msg, (int)strlen(msg), 0);
        char buf[64];
        int r = recv(c[i], buf, sizeof buf - 1, 0);
        buf[r > 0 ? r : 0] = 0;
        printf("client %d got: %s", i, buf);
        send(c[i], "and back\n", 9, 0);
        r = recv(s, buf, sizeof buf - 1, 0);
        buf[r > 0 ? r : 0] = 0;
        printf("server got: %s", buf);
        closesocket(c[i]);
        printf("after the client closed: %d\n", recv(s, buf, sizeof buf, 0));
        closesocket(s);
    }
    closesocket(l);
    SOCKET again = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    int r = connect(again, (struct sockaddr *)&a, sizeof a);
    int e = WSAGetLastError();
    printf("listener gone: %d error %d\n", r, e);
    closesocket(again);
}

static void udp(void)
{
    printf("--- UDP\n");
    SOCKET s = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
    struct sockaddr_in a = at(server, PORT);
    printf("sendto: %d\n", sendto(s, "abcdef", 6, 0, (struct sockaddr *)&a, sizeof a));
    DWORD ms = 5000;
    setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, (char *)&ms, sizeof ms);
    char buf[64];
    struct sockaddr_in from;
    int flen = sizeof from;
    int r = recvfrom(s, buf, sizeof buf - 1, 0, (struct sockaddr *)&from, &flen);
    buf[r > 0 ? r : 0] = 0;
    printf("recvfrom: %d %s, from port %d\n", r, buf, ntohs(from.sin_port));
    ms = 200;
    setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, (char *)&ms, sizeof ms);
    r = recvfrom(s, buf, sizeof buf, 0, NULL, NULL);
    int e = WSAGetLastError();
    printf("timed out: %d error %d\n", r, e);
    closesocket(s);
}

int main(int argc, char **argv)
{
    if (argc > 1)
        server = argv[1];
    setvbuf(stdout, NULL, _IONBF, 0);
    startup();
    names();
    conversation();
    refused();
    reset();
    nonblocking();
    loopback();
    udp();
    printf("cleanup: %d\n", WSACleanup());
    printf("done\n");
    return 6;
}
