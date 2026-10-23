/* Target F: Winsock over the OSyx network stack: WSAStartup, name
 * resolution, TCP echo with the host test server (10.0.2.2), a listening
 * socket on loopback, UDP, error codes, WSACleanup.
 * Usage: winsock.exe <tcp echo port> <udp echo port> */
#include <winsock2.h> /* before windows.h (common.h) */
#include "common.h"

static int parse_port(const char *s) {
    int v = 0;
    while (*s >= '0' && *s <= '9') v = v * 10 + (*s++ - '0');
    return v;
}

/* The two numbers after the program name on the command line. */
static void ports(int *tcp, int *udp) {
    const char *c = GetCommandLineA();
    while (*c && *c != ' ') c++;
    while (*c == ' ') c++;
    *tcp = parse_port(c);
    while (*c && *c != ' ') c++;
    while (*c == ' ') c++;
    *udp = parse_port(c);
}

void __stdcall start(void) {
    int tcp_port, udp_port;
    ports(&tcp_port, &udp_port);
    WSADATA wsa;
    check_eq("WSAStartup(2.2)", WSAStartup(MAKEWORD(2, 2), &wsa), 0);
    check_eq("negotiated version", wsa.wVersion, MAKEWORD(2, 2));

    struct hostent *he = gethostbyname("example.com");
    check("gethostbyname(\"example.com\") through the OSyx resolver", he != NULL && he->h_length == 4 && he->h_addr_list[0] != NULL);
    he = gethostbyname("no-such-host.invalid");
    check("unknown host fails", he == NULL);
    check_eq("... with WSAHOST_NOT_FOUND", WSAGetLastError(), WSAHOST_NOT_FOUND);

    SOCKET s = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    check("socket(AF_INET, SOCK_STREAM)", s != INVALID_SOCKET);
    struct sockaddr_in a = {0};
    a.sin_family = AF_INET;
    a.sin_port = htons((u_short)tcp_port);
    a.sin_addr.s_addr = inet_addr("10.0.2.2");
    check_eq("connect to the host echo server", connect(s, (struct sockaddr *)&a, sizeof a), 0);
    static const char msg[] = "hello from a Winsock program on OSyx";
    check_eq("send", send(s, msg, sizeof msg - 1, 0), sizeof msg - 1);
    char buf[128];
    int got = 0;
    while (got < (int)sizeof msg - 1) {
        int n = recv(s, buf + got, sizeof buf - got, 0);
        if (n <= 0) break;
        got += n;
    }
    check("recv returns the echo", got == sizeof msg - 1 && mem_eq(buf, msg, got));
    check_eq("closesocket", closesocket(s), 0);
    check_eq("closesocket twice fails", closesocket(s), SOCKET_ERROR);
    check_eq("... with WSAENOTSOCK", WSAGetLastError(), WSAENOTSOCK);

    /* Server and client on loopback (both in this thread: the client's
     * connect completes against the listen backlog). */
    SOCKET l = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in la = {0};
    la.sin_family = AF_INET;
    la.sin_port = htons(8123);
    la.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    check_eq("bind 127.0.0.1:8123", bind(l, (struct sockaddr *)&la, sizeof la), 0);
    check_eq("listen", listen(l, 4), 0);
    SOCKET c = socket(AF_INET, SOCK_STREAM, 0);
    check_eq("connect over loopback", connect(c, (struct sockaddr *)&la, sizeof la), 0);
    struct sockaddr_in peer;
    int plen = sizeof peer;
    SOCKET srv = accept(l, (struct sockaddr *)&peer, &plen);
    check("accept", srv != INVALID_SOCKET && peer.sin_addr.s_addr == htonl(INADDR_LOOPBACK));
    check("loopback data", send(c, "ping", 4, 0) == 4 && recv(srv, buf, 4, 0) == 4 && mem_eq(buf, "ping", 4));
    closesocket(srv);
    closesocket(c);
    closesocket(l);

    /* Connection refused by a closed loopback port. */
    s = socket(AF_INET, SOCK_STREAM, 0);
    la.sin_port = htons(8124);
    check_eq("connect to a closed port fails", connect(s, (struct sockaddr *)&la, sizeof la), SOCKET_ERROR);
    check_eq("... with WSAECONNREFUSED", WSAGetLastError(), WSAECONNREFUSED);
    closesocket(s);

    /* UDP echo with the host. */
    SOCKET u = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
    a.sin_port = htons((u_short)udp_port);
    check_eq("sendto (UDP)", sendto(u, "udp!", 4, 0, (struct sockaddr *)&a, sizeof a), 4);
    DWORD timeout = 3000;
    setsockopt(u, SOL_SOCKET, SO_RCVTIMEO, (const char *)&timeout, sizeof timeout);
    struct sockaddr_in from;
    int flen = sizeof from;
    int n = recvfrom(u, buf, sizeof buf, 0, (struct sockaddr *)&from, &flen);
    check("recvfrom gets the echo from 10.0.2.2", n == 4 && mem_eq(buf, "udp!", 4) && from.sin_addr.s_addr == inet_addr("10.0.2.2"));
    closesocket(u);
    check_eq("WSACleanup", WSACleanup(), 0);
    finish("winsock");
}
