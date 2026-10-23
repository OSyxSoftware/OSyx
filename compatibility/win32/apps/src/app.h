/* Shared helpers of the OSyx Windows applications (no C runtime): memory,
 * strings, drawing helpers and the dark colour scheme. The applications
 * use only documented Win32 APIs, so the same .exe runs on Windows and on
 * OSyx (under winrun). */
#ifndef OSYX_APP_H
#define OSYX_APP_H
#ifndef UNICODE
#define UNICODE
#define _UNICODE
#endif
#include <windows.h>

void *memset(void *d, int c, size_t n) {
    unsigned char *p = d;
    while (n--) *p++ = (unsigned char)c;
    return d;
}

void *memcpy(void *d, const void *s, size_t n) {
    unsigned char *p = d;
    const unsigned char *q = s;
    while (n--) *p++ = *q++;
    return d;
}

void *memmove(void *d, const void *s, size_t n) {
    unsigned char *p = d;
    const unsigned char *q = s;
    if (p < q) {
        while (n--) *p++ = *q++;
    } else {
        p += n;
        q += n;
        while (n--) *--p = *--q;
    }
    return d;
}

static __attribute__((unused)) void *mem_alloc(SIZE_T n) { return HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, n ? n : 1); }
static __attribute__((unused)) void *mem_realloc(void *p, SIZE_T n) { return p ? HeapReAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, p, n ? n : 1) : mem_alloc(n); }
static __attribute__((unused)) void mem_free(void *p) { if (p) HeapFree(GetProcessHeap(), 0, p); }

static __attribute__((unused)) int wlen(const WCHAR *s) { int n = 0; while (s && s[n]) n++; return n; }

static __attribute__((unused)) void wcopy(WCHAR *d, const WCHAR *s, int cap) {
    int i = 0;
    for (; s && s[i] && i < cap - 1; i++) d[i] = s[i];
    d[i] = 0;
}

static __attribute__((unused)) void wcat(WCHAR *d, const WCHAR *s, int cap) {
    int n = wlen(d);
    wcopy(d + n, s, cap - n);
}

/* Unsigned decimal into `out` (returns the length). */
static __attribute__((unused)) int wuint(WCHAR *out, unsigned long long v) {
    WCHAR tmp[24];
    int n = 0, i = 0;
    do {
        tmp[n++] = (WCHAR)(L'0' + v % 10);
        v /= 10;
    } while (v);
    while (n) out[i++] = tmp[--n];
    out[i] = 0;
    return i;
}

/* "12.3 GB" style sizes. */
static __attribute__((unused)) void wsize(WCHAR *out, unsigned long long b) {
    static const WCHAR *units[] = {L"B", L"KB", L"MB", L"GB", L"TB"};
    int u = 0;
    unsigned long long whole = b, frac = 0;
    while (whole >= 1024 && u < 4) {
        frac = (whole % 1024) * 10 / 1024;
        whole /= 1024;
        u++;
    }
    int n = wuint(out, whole);
    if (u > 0 && whole < 100) {
        out[n++] = L'.';
        n += wuint(out + n, frac);
    }
    out[n++] = L' ';
    wcopy(out + n, units[u], 8);
}

/* Colours (dark scheme close to the OSyx desktop). */
#define C_BG RGB(24, 26, 33)
#define C_PANEL RGB(32, 35, 44)
#define C_CARD RGB(38, 41, 52)
#define C_BORDER RGB(56, 60, 74)
#define C_TEXT RGB(226, 230, 238)
#define C_DIM RGB(150, 156, 172)
#define C_FAINT RGB(100, 106, 122)
#define C_ACCENT RGB(76, 141, 255)
#define C_SEL RGB(44, 62, 100)

static __attribute__((unused)) void fill(HDC dc, int x, int y, int w, int h, COLORREF c) {
    RECT r = {x, y, x + w, y + h};
    HBRUSH b = CreateSolidBrush(c);
    FillRect(dc, &r, b);
    DeleteObject(b);
}

static __attribute__((unused)) void text(HDC dc, int x, int y, const WCHAR *s, COLORREF c) {
    SetBkMode(dc, TRANSPARENT);
    SetTextColor(dc, c);
    TextOutW(dc, x, y, s, wlen(s));
}

static __attribute__((unused)) void text_right(HDC dc, int right, int y, const WCHAR *s, COLORREF c) {
    SIZE sz;
    GetTextExtentPoint32W(dc, s, wlen(s), &sz);
    text(dc, right - sz.cx, y, s, c);
}

static __attribute__((unused)) HFONT ui_font(int height, int bold) {
    return CreateFontW(-height, 0, 0, 0, bold ? FW_BOLD : FW_NORMAL, 0, 0, 0, DEFAULT_CHARSET, 0, 0, CLEARTYPE_QUALITY, VARIABLE_PITCH | FF_SWISS, L"Segoe UI");
}

static __attribute__((unused)) HFONT mono_font(int height) {
    return CreateFontW(-height, 0, 0, 0, FW_NORMAL, 0, 0, 0, DEFAULT_CHARSET, 0, 0, CLEARTYPE_QUALITY, FIXED_PITCH | FF_MODERN, L"Consolas");
}

/* The first command-line argument after the program name (or NULL). */
static __attribute__((unused)) const WCHAR *first_arg(WCHAR *buf, int cap) {
    const WCHAR *c = GetCommandLineW();
    if (*c == L'"') {
        c++;
        while (*c && *c != L'"') c++;
        if (*c) c++;
    } else {
        while (*c && *c != L' ') c++;
    }
    while (*c == L' ') c++;
    if (!*c) return NULL;
    int i = 0;
    if (*c == L'"') {
        c++;
        while (*c && *c != L'"' && i < cap - 1) buf[i++] = *c++;
    } else {
        while (*c && i < cap - 1) buf[i++] = *c++;
    }
    while (i > 0 && buf[i - 1] == L' ') i--;
    buf[i] = 0;
    return buf;
}

#endif
