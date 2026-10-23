/* OSyx Network Center (netcenter.exe): network adapters with their
 * addresses, gateway and DNS servers, live send/receive throughput, and
 * two tools: ping (ICMP echo) and DNS lookup. Documented APIs only
 * (iphlpapi: GetAdaptersInfo, GetIfTable, GetNetworkParams, IcmpSendEcho;
 * Winsock: getaddrinfo), so it runs on Windows and on OSyx (winrun),
 * where the data comes from the OSyx network stack. No C runtime. */
#define UNICODE
#define _UNICODE
#include <winsock2.h>
#include <ws2tcpip.h>
#include "app.h"
#include <iphlpapi.h>
#include <icmpapi.h>

#define HIST 60
#define MAX_AD 8

typedef struct {
    WCHAR name[132], desc[132], ip[64], mask[32], gw[32], mac[32];
    BOOL dhcp;
    DWORD index;
    unsigned long long rx, tx; /* totals */
    float rx_hist[HIST], tx_hist[HIST];
} Adapter;

static Adapter ads[MAX_AD];
static int nads, sel;
static WCHAR host_name[132], dns[132];
static WCHAR target[128] = L"10.0.2.2";
static WCHAR lookup[128] = L"example.com";
static int focus; /* 0 none, 1 ping field, 2 dns field */
static WCHAR results[12][128];
static int nresults;
static HFONT f_ui, f_bold, f_big;

static void ascii_to_w(WCHAR *d, const char *s, int cap) {
    int i = 0;
    for (; s[i] && i < cap - 1; i++) d[i] = (unsigned char)s[i];
    d[i] = 0;
}

static void w_to_ascii(char *d, const WCHAR *s, int cap) {
    int i = 0;
    for (; s[i] && i < cap - 1; i++) d[i] = (char)s[i];
    d[i] = 0;
}

static void add_result(const WCHAR *s) {
    if (nresults == 12) {
        memmove(results[0], results[1], sizeof(results[0]) * 11);
        nresults = 11;
    }
    wcopy(results[nresults++], s, 128);
}

static void ip4(WCHAR *out, const unsigned char *a) {
    int n = 0;
    for (int i = 0; i < 4; i++) {
        n += wuint(out + n, a[i]);
        if (i < 3) out[n++] = L'.';
    }
    out[n] = 0;
}

static void scan_adapters(void) {
    ULONG len = 0;
    GetAdaptersInfo(NULL, &len);
    IP_ADAPTER_INFO *info = mem_alloc(len ? len : sizeof(IP_ADAPTER_INFO));
    int n = 0;
    if (GetAdaptersInfo(info, &len) == NO_ERROR) {
        for (IP_ADAPTER_INFO *a = info; a && n < MAX_AD; a = a->Next) {
            Adapter *d = &ads[n];
            DWORD keep_index = d->index;
            if (keep_index != a->Index) memset(d, 0, sizeof *d);
            d->index = a->Index;
            ascii_to_w(d->name, a->AdapterName, 132);
            ascii_to_w(d->desc, a->Description, 132);
            ascii_to_w(d->ip, a->IpAddressList.IpAddress.String, 64);
            ascii_to_w(d->mask, a->IpAddressList.IpMask.String, 32);
            ascii_to_w(d->gw, a->GatewayList.IpAddress.String, 32);
            d->dhcp = a->DhcpEnabled;
            static const WCHAR hex[] = L"0123456789ABCDEF";
            int m = 0;
            for (UINT i = 0; i < a->AddressLength && i < 8; i++) {
                d->mac[m++] = hex[a->Address[i] >> 4];
                d->mac[m++] = hex[a->Address[i] & 15];
                if (i + 1 < a->AddressLength) d->mac[m++] = L'-';
            }
            d->mac[m] = 0;
            n++;
        }
    }
    mem_free(info);
    nads = n;
    if (sel >= nads) sel = 0;
    FIXED_INFO *fi = mem_alloc(sizeof(FIXED_INFO) * 2);
    ULONG fl = sizeof(FIXED_INFO) * 2;
    if (GetNetworkParams(fi, &fl) == NO_ERROR) {
        ascii_to_w(host_name, fi->HostName, 132);
        ascii_to_w(dns, fi->DnsServerList.IpAddress.String, 132);
    }
    mem_free(fi);
}

/* Byte counters per adapter (GetIfTable), pushed into the histories. */
static void sample(void) {
    ULONG len = 0;
    GetIfTable(NULL, &len, FALSE);
    MIB_IFTABLE *t = mem_alloc(len ? len : sizeof(MIB_IFTABLE));
    if (GetIfTable(t, &len, FALSE) == NO_ERROR) {
        for (int i = 0; i < nads; i++) {
            Adapter *d = &ads[i];
            for (DWORD k = 0; k < t->dwNumEntries; k++) {
                MIB_IFROW *r = &t->table[k];
                if (r->dwIndex != d->index) continue;
                unsigned long long rx = r->dwInOctets, tx = r->dwOutOctets;
                float drx = d->rx && rx >= d->rx ? (float)(rx - d->rx) : 0, dtx = d->tx && tx >= d->tx ? (float)(tx - d->tx) : 0;
                d->rx = rx;
                d->tx = tx;
                memmove(d->rx_hist, d->rx_hist + 1, sizeof(float) * (HIST - 1));
                memmove(d->tx_hist, d->tx_hist + 1, sizeof(float) * (HIST - 1));
                d->rx_hist[HIST - 1] = drx;
                d->tx_hist[HIST - 1] = dtx;
            }
        }
    }
    mem_free(t);
}

static void do_ping(void) {
    char host[128];
    w_to_ascii(host, target, 128);
    struct addrinfo hints, *res = NULL;
    memset(&hints, 0, sizeof hints);
    hints.ai_family = AF_INET;
    WCHAR line[128];
    if (getaddrinfo(host, NULL, &hints, &res) != 0 || !res) {
        wcopy(line, L"Ping: cannot resolve ", 128);
        wcat(line, target, 128);
        add_result(line);
        return;
    }
    IPAddr addr = ((struct sockaddr_in *)res->ai_addr)->sin_addr.s_addr;
    freeaddrinfo(res);
    HANDLE icmp = IcmpCreateFile();
    if (icmp == INVALID_HANDLE_VALUE) {
        add_result(L"Ping: ICMP is not available");
        return;
    }
    char payload[32] = "OSyx Network Center ping";
    char reply[sizeof(ICMP_ECHO_REPLY) + 64];
    for (int i = 0; i < 3; i++) {
        DWORD n = IcmpSendEcho(icmp, addr, payload, sizeof payload, NULL, reply, sizeof reply, 2000);
        WCHAR ipw[32];
        ip4(ipw, (unsigned char *)&addr);
        if (n > 0) {
            ICMP_ECHO_REPLY *r = (ICMP_ECHO_REPLY *)reply;
            wcopy(line, L"Reply from ", 128);
            wcat(line, ipw, 128);
            wcat(line, L": time=", 128);
            if (r->RoundTripTime == 0) wcat(line, L"<1", 128);
            else wuint(line + wlen(line), r->RoundTripTime);
            wcat(line, L" ms TTL=", 128);
            wuint(line + wlen(line), r->Options.Ttl);
        } else {
            wcopy(line, L"No reply from ", 128);
            wcat(line, ipw, 128);
            wcat(line, L" (error ", 128);
            wuint(line + wlen(line), GetLastError());
            wcat(line, L")", 128);
        }
        add_result(line);
    }
    IcmpCloseHandle(icmp);
}

static void do_lookup(void) {
    char host[128];
    w_to_ascii(host, lookup, 128);
    struct addrinfo hints, *res = NULL;
    memset(&hints, 0, sizeof hints);
    hints.ai_family = AF_INET;
    WCHAR line[128];
    int rc = getaddrinfo(host, NULL, &hints, &res);
    if (rc != 0) {
        wcopy(line, L"DNS: ", 128);
        wcat(line, lookup, 128);
        wcat(line, L" not found (", 128);
        wuint(line + wlen(line), rc);
        wcat(line, L")", 128);
        add_result(line);
        return;
    }
    for (struct addrinfo *a = res; a; a = a->ai_next) {
        WCHAR ipw[32];
        ip4(ipw, (unsigned char *)&((struct sockaddr_in *)a->ai_addr)->sin_addr);
        wcopy(line, lookup, 128);
        wcat(line, L" → ", 128);
        wcat(line, ipw, 128);
        add_result(line);
    }
    freeaddrinfo(res);
}

static void rate(WCHAR *out, float bps) {
    unsigned long long kbit = (unsigned long long)(bps * 8 / 1000);
    if (kbit >= 1000) {
        int n = wuint(out, kbit / 1000);
        out[n++] = L'.';
        n += wuint(out + n, (kbit % 1000) / 100);
        wcopy(out + n, L" Mbps", 8);
    } else {
        int n = wuint(out, kbit);
        wcopy(out + n, L" Kbps", 8);
    }
}

static void graph(HDC dc, int x, int y, int w, int h, const float *a, const float *b) {
    fill(dc, x, y, w, h, RGB(16, 18, 24));
    for (int i = 1; i < 6; i++) fill(dc, x + w * i / 6, y, 1, h, RGB(34, 38, 48));
    for (int i = 1; i < 4; i++) fill(dc, x, y + h * i / 4, w, 1, RGB(34, 38, 48));
    float top = 2048;
    for (int i = 0; i < HIST; i++) {
        if (a[i] > top) top = a[i];
        if (b[i] > top) top = b[i];
    }
    for (int i = 0; i < HIST; i++) {
        int cx = x + i * w / HIST, cw = w / HIST > 1 ? w / HIST - 1 : 1;
        int ha = (int)(a[i] / top * (h - 4)), hb = (int)(b[i] / top * (h - 4));
        fill(dc, cx, y + h - ha, cw, ha, RGB(230, 120, 170));
        if (hb) fill(dc, cx, y + h - hb, cw > 2 ? cw / 2 : cw, hb, RGB(240, 200, 80));
    }
}

static void field(HDC dc, int x, int y, int w, const WCHAR *s, int focused) {
    fill(dc, x, y, w, 30, RGB(20, 22, 28));
    fill(dc, x, y + 28, w, 2, focused ? C_ACCENT : C_BORDER);
    text(dc, x + 8, y + 6, s, C_TEXT);
}

static void paint(HWND hwnd) {
    PAINTSTRUCT ps;
    HDC dc = BeginPaint(hwnd, &ps);
    RECT r;
    GetClientRect(hwnd, &r);
    int w = r.right, h = r.bottom, split = w * 55 / 100;
    fill(dc, 0, 0, w, h, C_BG);
    SelectObject(dc, f_big);
    text(dc, 20, 14, L"Network Center", C_TEXT);
    SelectObject(dc, f_ui);
    WCHAR sub[300] = L"Host ";
    wcat(sub, host_name[0] ? host_name : L"?", 300);
    wcat(sub, L"  ·  DNS ", 300);
    wcat(sub, dns[0] ? dns : L"none", 300);
    text(dc, 22, 46, sub, C_DIM);
    /* Adapter tabs. */
    for (int i = 0; i < nads; i++) {
        int x = 20 + i * 150;
        fill(dc, x, 76, 140, 30, i == sel ? C_SEL : C_PANEL);
        WCHAR t[20];
        wcopy(t, L"Adapter ", 20);
        wuint(t + wlen(t), i + 1);
        text(dc, x + 12, 82, t, C_TEXT);
    }
    if (nads == 0) text(dc, 20, 82, L"No network adapters", C_DIM);
    int y0 = 118;
    if (nads > 0) {
        Adapter *d = &ads[sel];
        fill(dc, 20, y0, split - 30, 200, C_CARD);
        SelectObject(dc, f_bold);
        text(dc, 36, y0 + 12, d->desc, C_TEXT);
        SelectObject(dc, f_ui);
        const WCHAR *keys[] = {L"IPv4 address", L"Subnet mask", L"Default gateway", L"Physical address", L"Configuration"};
        const WCHAR *vals[] = {d->ip, d->mask, d->gw, d->mac, d->dhcp ? L"DHCP" : L"Static"};
        for (int i = 0; i < 5; i++) {
            text(dc, 36, y0 + 44 + i * 28, keys[i], C_DIM);
            text_right(dc, split - 26, y0 + 44 + i * 28, vals[i][0] ? vals[i] : L"-", C_TEXT);
        }
        SelectObject(dc, f_bold);
        text(dc, 20, y0 + 216, L"Throughput (60 s)", C_TEXT);
        SelectObject(dc, f_ui);
        WCHAR a[32], b[32], line[96] = L"Receive ";
        rate(a, d->rx_hist[HIST - 1]);
        rate(b, d->tx_hist[HIST - 1]);
        wcat(line, a, 96);
        wcat(line, L"   Send ", 96);
        wcat(line, b, 96);
        text_right(dc, split - 10, y0 + 216, line, C_DIM);
        int gh = h - (y0 + 244) - 16;
        if (gh > 40) graph(dc, 20, y0 + 244, split - 30, gh, d->rx_hist, d->tx_hist);
    }
    /* Tools. */
    int x = split + 10, cw = w - x - 20;
    SelectObject(dc, f_bold);
    text(dc, x, y0 - 40, L"Ping", C_TEXT);
    SelectObject(dc, f_ui);
    field(dc, x, y0 - 14, cw - 100, target, focus == 1);
    fill(dc, x + cw - 90, y0 - 14, 90, 30, C_ACCENT);
    text(dc, x + cw - 64, y0 - 8, L"Ping", RGB(255, 255, 255));
    SelectObject(dc, f_bold);
    text(dc, x, y0 + 30, L"DNS lookup", C_TEXT);
    SelectObject(dc, f_ui);
    field(dc, x, y0 + 56, cw - 100, lookup, focus == 2);
    fill(dc, x + cw - 90, y0 + 56, 90, 30, C_ACCENT);
    text(dc, x + cw - 72, y0 + 62, L"Lookup", RGB(255, 255, 255));
    fill(dc, x, y0 + 100, cw, h - y0 - 116, C_PANEL);
    for (int i = 0; i < nresults; i++) text(dc, x + 12, y0 + 110 + i * 22, results[i], C_TEXT);
    if (!nresults) text(dc, x + 12, y0 + 110, L"Results appear here.", C_FAINT);
    EndPaint(hwnd, &ps);
}

static LRESULT CALLBACK wndproc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp) {
    switch (msg) {
    case WM_CREATE: {
        f_ui = ui_font(14, 0);
        f_bold = ui_font(14, 1);
        f_big = ui_font(24, 0);
        WSADATA wsa;
        WSAStartup(MAKEWORD(2, 2), &wsa);
        scan_adapters();
        sample();
        SetTimer(hwnd, 1, 1000, NULL);
        return 0;
    }
    case WM_TIMER:
        sample();
        InvalidateRect(hwnd, NULL, FALSE);
        return 0;
    case WM_PAINT:
        paint(hwnd);
        return 0;
    case WM_ERASEBKGND:
        return 1;
    case WM_SIZE:
        InvalidateRect(hwnd, NULL, FALSE);
        return 0;
    case WM_LBUTTONDOWN: {
        RECT r;
        GetClientRect(hwnd, &r);
        int x = (short)LOWORD(lp), y = (short)HIWORD(lp), split = r.right * 55 / 100, cw = r.right - split - 30, fx = split + 10;
        focus = 0;
        if (y >= 76 && y < 106 && x >= 20 && (x - 20) / 150 < nads) sel = (x - 20) / 150;
        else if (y >= 104 && y < 134 && x >= fx && x < fx + cw - 100) focus = 1;
        else if (y >= 104 && y < 134 && x >= fx + cw - 90 && x < fx + cw) do_ping();
        else if (y >= 174 && y < 204 && x >= fx && x < fx + cw - 100) focus = 2;
        else if (y >= 174 && y < 204 && x >= fx + cw - 90 && x < fx + cw) do_lookup();
        InvalidateRect(hwnd, NULL, FALSE);
        return 0;
    }
    case WM_CHAR: {
        WCHAR *f = focus == 1 ? target : focus == 2 ? lookup : NULL;
        if (!f) return 0;
        int n = wlen(f);
        if (wp == 8) {
            if (n) f[n - 1] = 0;
        } else if (wp == L'\r') {
            if (focus == 1) do_ping();
            else do_lookup();
        } else if (wp >= 32 && n < 127) {
            f[n] = (WCHAR)wp;
            f[n + 1] = 0;
        }
        InvalidateRect(hwnd, NULL, FALSE);
        return 0;
    }
    case WM_KEYDOWN:
        if (wp == VK_F5) {
            scan_adapters();
            InvalidateRect(hwnd, NULL, FALSE);
        }
        return 0;
    case WM_DESTROY:
        WSACleanup();
        PostQuitMessage(0);
        return 0;
    }
    return DefWindowProcW(hwnd, msg, wp, lp);
}

void __stdcall start(void) {
    WNDCLASSW wc = {0};
    wc.lpfnWndProc = wndproc;
    wc.hInstance = GetModuleHandleW(NULL);
    wc.hCursor = LoadCursorW(NULL, IDC_ARROW);
    wc.lpszClassName = L"OSyxNetworkCenter";
    RegisterClassW(&wc);
    HWND hwnd = CreateWindowExW(0, L"OSyxNetworkCenter", L"OSyx Network Center", WS_OVERLAPPEDWINDOW, CW_USEDEFAULT, CW_USEDEFAULT, 1000, 660, NULL, NULL, wc.hInstance, NULL);
    if (!hwnd) ExitProcess(1);
    ShowWindow(hwnd, SW_SHOW);
    UpdateWindow(hwnd);
    MSG m;
    while (GetMessageW(&m, NULL, 0, 0) > 0) {
        TranslateMessage(&m);
        DispatchMessageW(&m);
    }
    ExitProcess((UINT)m.wParam);
}
