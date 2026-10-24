/* Legacy Winsock 1.1 through wsock32.dll, as NSPR (nss3.dll) uses it:
 * WSAStartup(1.1), every ordinal nss3 imports, FD_ISSET (__WSAFDIsSet),
 * getsockopt, protocol and loopback address lookups. */
#include <winsock2.h> /* before windows.h (common.h) */
#include "common.h"

/* The ordinals nss3.dll of Zen 1.23.4b imports from wsock32.dll. */
static const WORD nss3_ordinals[] = {
    1, 2, 3, 4, 5, 6, 7, 8, 9, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
    51, 52, 53, 54, 57, 111, 115, 116, 151,
};

void __stdcall start(void) {
    WSADATA wsa;
    check_eq("WSAStartup(1.1)", WSAStartup(MAKEWORD(1, 1), &wsa), 0);
    check_eq("negotiated version 1.1", wsa.wVersion, MAKEWORD(1, 1));
    check_eq("highest version 2.2", wsa.wHighVersion, MAKEWORD(2, 2));

    HMODULE w = LoadLibraryA("wsock32.dll");
    check("LoadLibrary(wsock32.dll)", w != NULL);
    int missing = 0;
    for (unsigned i = 0; i < sizeof nss3_ordinals / sizeof nss3_ordinals[0]; i++)
        if (!GetProcAddress(w, MAKEINTRESOURCEA(nss3_ordinals[i]))) missing++;
    check_eq("the 30 ordinals nss3.dll imports resolve", missing, 0);
    check("ordinal 115 is WSAStartup", (FARPROC)GetProcAddress(w, MAKEINTRESOURCEA(115)) == GetProcAddress(w, "WSAStartup"));
    HMODULE modern=LoadLibraryA("ws2_32.dll");
    check("legacy ordinal 10 is inet_addr",GetProcAddress(w,MAKEINTRESOURCEA(10))==GetProcAddress(w,"inet_addr"));
    check("legacy ordinal 11 is inet_ntoa",GetProcAddress(w,MAKEINTRESOURCEA(11))==GetProcAddress(w,"inet_ntoa"));
    check("legacy ordinal 12 is ioctlsocket",GetProcAddress(w,MAKEINTRESOURCEA(12))==GetProcAddress(w,"ioctlsocket"));
    check("Winsock 2 ordinal 10 is ioctlsocket",modern && GetProcAddress(modern,MAKEINTRESOURCEA(10))==GetProcAddress(modern,"ioctlsocket"));
    check("Winsock 2 ordinal 11 is inet_addr",modern && GetProcAddress(modern,MAKEINTRESOURCEA(11))==GetProcAddress(modern,"inet_addr"));
    check("Winsock 2 ordinal 12 is inet_ntoa",modern && GetProcAddress(modern,MAKEINTRESOURCEA(12))==GetProcAddress(modern,"inet_ntoa"));

    struct protoent *pe = getprotobyname("tcp");
    check("getprotobyname(\"tcp\") is 6", pe != NULL && pe->p_proto == 6 && mem_eq(pe->p_name, "tcp", 4));
    pe = getprotobynumber(17);
    check("getprotobynumber(17) is udp", pe != NULL && pe->p_proto == 17 && mem_eq(pe->p_name, "udp", 4));
    check("an unknown protocol is not found", getprotobyname("no-such-protocol") == NULL);

    struct in_addr lo;
    lo.s_addr = htonl(INADDR_LOOPBACK);
    struct hostent *he = gethostbyaddr((const char *)&lo, 4, AF_INET);
    check("gethostbyaddr(127.0.0.1) names this machine", he != NULL && he->h_addrtype == AF_INET && he->h_length == 4 && he->h_name[0] != 0);

    SOCKET l = socket(AF_INET, SOCK_STREAM, 0);
    SOCKET u = socket(AF_INET, SOCK_DGRAM, 0);
    typedef int (WSAAPI *ioctl_fn)(SOCKET,long,u_long*);
    ioctl_fn legacy_ioctl=(ioctl_fn)GetProcAddress(w,MAKEINTRESOURCEA(12));
    u_long nonblocking=1;
    check_eq("NSPR ordinal call enables nonblocking I/O",legacy_ioctl(u,FIONBIO,&nonblocking),0);
    nonblocking=0;
    check_eq("NSPR ordinal call restores blocking I/O",legacy_ioctl(u,FIONBIO,&nonblocking),0);
    int type = 0, len = sizeof type;
    check("getsockopt(SO_TYPE) of a TCP socket", getsockopt(l, SOL_SOCKET, SO_TYPE, (char *)&type, &len) == 0 && type == SOCK_STREAM && len == 4);
    len = sizeof type;
    check("getsockopt(SO_TYPE) of a UDP socket", getsockopt(u, SOL_SOCKET, SO_TYPE, (char *)&type, &len) == 0 && type == SOCK_DGRAM);
    int e = -1;
    len = sizeof e;
    check("getsockopt(SO_ERROR) is 0", getsockopt(l, SOL_SOCKET, SO_ERROR, (char *)&e, &len) == 0 && e == 0);

    /* select reports the listener readable once a client is in its backlog. */
    struct sockaddr_in la = {0};
    la.sin_family = AF_INET;
    la.sin_port = htons(8125);
    la.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    check_eq("bind 127.0.0.1:8125", bind(l, (struct sockaddr *)&la, sizeof la), 0);
    check_eq("listen", listen(l, 4), 0);
    SOCKET c = socket(AF_INET, SOCK_STREAM, 0);
    check_eq("connect over loopback", connect(c, (struct sockaddr *)&la, sizeof la), 0);
    struct sockaddr_in endpoint={0};
    len=sizeof endpoint;
    check("listener getsockname reports the bound endpoint",getsockname(l,(struct sockaddr*)&endpoint,&len)==0 && len==sizeof endpoint && endpoint.sin_family==AF_INET && endpoint.sin_port==la.sin_port && endpoint.sin_addr.s_addr==la.sin_addr.s_addr);
    len=sizeof endpoint;
    check("connected getsockname reports its ephemeral endpoint",getsockname(c,(struct sockaddr*)&endpoint,&len)==0 && endpoint.sin_family==AF_INET && endpoint.sin_port!=0 && endpoint.sin_addr.s_addr==la.sin_addr.s_addr);
    len=sizeof endpoint;
    check("getpeername reports the remote endpoint",getpeername(c,(struct sockaddr*)&endpoint,&len)==0 && endpoint.sin_family==AF_INET && endpoint.sin_port==la.sin_port && endpoint.sin_addr.s_addr==la.sin_addr.s_addr);
    len=sizeof endpoint-1;
    check("getsockname rejects a short buffer",getsockname(c,(struct sockaddr*)&endpoint,&len)==SOCKET_ERROR && WSAGetLastError()==WSAEFAULT);
    len=sizeof endpoint;
    check("getpeername rejects a null address",getpeername(c,NULL,&len)==SOCKET_ERROR && WSAGetLastError()==WSAEFAULT);
    check("getsockname rejects a null length",getsockname(c,(struct sockaddr*)&endpoint,NULL)==SOCKET_ERROR && WSAGetLastError()==WSAEFAULT);
    fd_set rd;
    FD_ZERO(&rd);
    FD_SET(l, &rd);
    FD_SET(u, &rd);
    struct timeval tv = {2, 0};
    check_eq("select finds one readable socket", select(0, &rd, NULL, NULL, &tv), 1);
    check("FD_ISSET of the listener", FD_ISSET(l, &rd));
    check("FD_ISSET of the idle UDP socket is false", !FD_ISSET(u, &rd));
    closesocket(c);
    closesocket(u);
    closesocket(l);
    check_eq("WSACleanup", WSACleanup(), 0);
    finish("wsock32");
}
