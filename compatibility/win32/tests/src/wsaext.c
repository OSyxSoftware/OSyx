/* The Winsock 2 extensions xul.dll and nss3.dll import: WSASocketW,
 * WSAIoctl(FIONBIO), WSASendMsg, inet_pton/inet_ntop and the provider
 * catalog (WSCEnumProtocols, WSCGetProviderInfo, WSCGetProviderPath). */
#include <winsock2.h> /* before windows.h (common.h) */
#include <ws2tcpip.h>
#include <ws2spi.h>
#include <mswsock.h>
#include "common.h"

static int str_eq(const char *a, const char *b) {
    while (*a && *a == *b) a++, b++;
    return *a == *b;
}

/* Whether the wide string ends with the ASCII `tail`, ignoring case. */
static int wide_ends_with(const WCHAR *w, const char *tail) {
    int n = 0, t = 0;
    while (w[n]) n++;
    while (tail[t]) t++;
    if (n < t) return 0;
    for (int i = 0; i < t; i++) {
        WCHAR c = w[n - t + i];
        if (c >= 'A' && c <= 'Z') c += 32;
        if (c != (WCHAR)tail[i]) return 0;
    }
    return 1;
}

static WSAPROTOCOL_INFOW catalog[64];

void __stdcall start(void) {
    WSADATA wsa;
    check_eq("WSAStartup(2.2)", WSAStartup(MAKEWORD(2, 2), &wsa), 0);
    check_eq("WSAPROTOCOL_INFOW is 628 bytes", sizeof(WSAPROTOCOL_INFOW), 628);
    check_eq("WSAMSG is 56 bytes", sizeof(WSAMSG), 56);

    /* inet_pton / inet_ntop */
    struct in_addr a4;
    struct in6_addr a6;
    char text[64];
    check_eq("inet_pton(AF_INET)", inet_pton(AF_INET, "192.168.1.20", &a4), 1);
    check("... network order", a4.s_addr == inet_addr("192.168.1.20"));
    check_eq("inet_pton rejects 1.2.3", inet_pton(AF_INET, "1.2.3", &a4), 0);
    check("inet_ntop(AF_INET)", inet_ntop(AF_INET, &a4, text, sizeof text) == text);
    check_eq("inet_pton(AF_INET6)", inet_pton(AF_INET6, "2001:DB8::8:800:200C:417A", &a6), 1);
    check("... bytes", a6.s6_addr[0] == 0x20 && a6.s6_addr[1] == 0x01 && a6.s6_addr[15] == 0x7a);
    check("inet_ntop(AF_INET6) is compressed lower case",
          inet_ntop(AF_INET6, &a6, text, sizeof text) != NULL && str_eq(text, "2001:db8::8:800:200c:417a"));
    inet_pton(AF_INET6, "::1", &a6);
    check("inet_ntop(::1)", inet_ntop(AF_INET6, &a6, text, sizeof text) != NULL && str_eq(text, "::1"));
    check("inet_ntop into a short buffer fails", inet_ntop(AF_INET6, &a6, text, 3) == NULL);

    /* WSASocketW + WSAIoctl(FIONBIO) + WSASendMsg over loopback UDP. */
    SOCKET r = WSASocketW(AF_INET, SOCK_DGRAM, IPPROTO_UDP, NULL, 0, WSA_FLAG_OVERLAPPED);
    SOCKET w = WSASocketW(AF_INET, SOCK_DGRAM, 0, NULL, 0, 0);
    check("WSASocketW(AF_INET, SOCK_DGRAM)", r != INVALID_SOCKET && w != INVALID_SOCKET);
    struct sockaddr_in la = {0};
    la.sin_family = AF_INET;
    la.sin_port = htons(8126);
    la.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    check_eq("bind 127.0.0.1:8126", bind(r, (struct sockaddr *)&la, sizeof la), 0);
    u_long on = 1;
    DWORD got = 99;
    check_eq("WSAIoctl(FIONBIO)", WSAIoctl(r, FIONBIO, &on, sizeof on, NULL, 0, &got, NULL, NULL), 0);
    char buf[32];
    check("an empty non-blocking socket would block",
          recv(r, buf, sizeof buf, 0) == SOCKET_ERROR && WSAGetLastError() == WSAEWOULDBLOCK);
    WSABUF parts[2] = {{5, "hello"}, {6, ", msg!"}};
    WSAMSG m = {0};
    m.name = (struct sockaddr *)&la;
    m.namelen = sizeof la;
    m.lpBuffers = parts;
    m.dwBufferCount = 2;
    DWORD sent = 0;
    check("WSASendMsg gathers two buffers", WSASendMsg(w, &m, 0, &sent, NULL, NULL) == 0 && sent == 11);
    on = 0;
    WSAIoctl(r, FIONBIO, &on, sizeof on, NULL, 0, &got, NULL, NULL);
    check("... into one datagram", recv(r, buf, sizeof buf, 0) == 11 && mem_eq(buf, "hello, msg!", 11));
    closesocket(w);
    closesocket(r);

    /* WSASend/WSARecv gather and scatter over loopback TCP. */
    SOCKET ls = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in ta = {0};
    ta.sin_family = AF_INET;
    ta.sin_port = htons(8127);
    ta.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    check_eq("bind 127.0.0.1:8127", bind(ls, (struct sockaddr *)&ta, sizeof ta), 0);
    check_eq("listen 8127", listen(ls, 4), 0);
    SOCKET cl = socket(AF_INET, SOCK_STREAM, 0);
    check_eq("connect 8127", connect(cl, (struct sockaddr *)&ta, sizeof ta), 0);
    SOCKET sv = accept(ls, NULL, NULL);
    check("accept 8127", sv != INVALID_SOCKET);
    WSABUF out[2] = {{5, "hello"}, {6, ", wsa!"}};
    DWORD n = 0;
    check("WSASend gathers two buffers", WSASend(cl, out, 2, &n, 0, NULL, NULL) == 0 && n == 11);
    DWORD avail = 0;
    check_eq("ioctlsocket(FIONREAD)", ioctlsocket(sv, FIONREAD, &avail), 0);
    check_eq("... pending bytes", avail, 11);
    avail = 0;
    n = 0;
    check_eq("WSAIoctl(FIONREAD)", WSAIoctl(sv, FIONREAD, NULL, 0, &avail, sizeof avail, &n, NULL, NULL), 0);
    check("... pending bytes and count", avail == 11 && n == 4);
    char part1[5] = {0}, part2[6] = {0};
    WSABUF in[2] = {{5, part1}, {6, part2}};
    n = 0;
    check("WSARecv scatters into two buffers", WSARecv(sv, in, 2, &n, NULL, NULL, NULL) == 0 && n == 11);
    check("... bytes", mem_eq(part1, "hello", 5) && mem_eq(part2, ", wsa!", 6));
    closesocket(sv);
    closesocket(cl);
    closesocket(ls);

    /* The provider catalog. */
    DWORD len = 0;
    int err = 0;
    check("WSCEnumProtocols without a buffer asks for one",
          WSCEnumProtocols(NULL, NULL, &len, &err) == SOCKET_ERROR && err == WSAENOBUFS && len >= 2 * sizeof(WSAPROTOCOL_INFOW));
    int tcp_only[] = {IPPROTO_TCP, 0};
    len = sizeof catalog;
    int n = WSCEnumProtocols(tcp_only, catalog, &len, &err);
    int found = -1;
    for (int i = 0; i < n; i++)
        if (catalog[i].iAddressFamily == AF_INET && catalog[i].iSocketType == SOCK_STREAM &&
            catalog[i].ProtocolChain.ChainLen == BASE_PROTOCOL)
            found = i;
    check("WSCEnumProtocols lists the IPv4 TCP base provider", n > 0 && found >= 0);
    int udp = 0;
    for (int i = 0; i < n; i++) udp |= catalog[i].iSocketType == SOCK_DGRAM;
    check("... and no UDP provider for {IPPROTO_TCP}", !udp);
    if (found >= 0) {
        GUID id = catalog[found].ProviderId;
        DWORD categories = 0xFFFFFFFF;
        size_t size = sizeof categories;
        check_eq("a base provider has no LSP categories",
                 WSCGetProviderInfo(&id, ProviderInfoLspCategories, (PBYTE)&categories, &size, 0, &err), SOCKET_ERROR);
        check_eq("... WSAEINVALIDPROVIDER", err, WSAEINVALIDPROVIDER);
        WCHAR path[MAX_PATH];
        int plen = MAX_PATH;
        check("WSCGetProviderPath is mswsock.dll",
              WSCGetProviderPath(&id, path, &plen, &err) == 0 && wide_ends_with(path, "mswsock.dll"));
    }
    GUID bogus = {0x12345678, 1, 2, {3, 4, 5, 6, 7, 8, 9, 10}};
    WCHAR path[MAX_PATH];
    int plen = MAX_PATH;
    check("WSCGetProviderPath of an unknown provider fails", WSCGetProviderPath(&bogus, path, &plen, &err) == SOCKET_ERROR);
    check_eq("WSACleanup", WSACleanup(), 0);
    finish("wsaext");
}
