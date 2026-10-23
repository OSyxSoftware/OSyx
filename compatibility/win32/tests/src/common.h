/* Shared helpers of the OSyx Windows test programs (no C runtime): output
 * through WriteFile and a tiny check counter. Every program prints
 * "<name>: <n> passed, <m> failed" last and exits with the failure count. */
#include <windows.h>

static int passed, failed;

/* The compiler may emit calls to these even without a C runtime. */
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

static DWORD str_len(const char *s) {
    DWORD n = 0;
    while (s[n]) n++;
    return n;
}

static void put(const char *s) {
    DWORD w;
    WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), s, str_len(s), &w, NULL);
}

static __attribute__((unused)) void put_uint(unsigned long long v) {
    char buf[24];
    int i = 23;
    buf[i] = 0;
    do {
        buf[--i] = (char)('0' + v % 10);
        v /= 10;
    } while (v);
    put(buf + i);
}

static void check(const char *what, int ok) {
    put(ok ? "  ok   " : "  FAIL ");
    put(what);
    put("\r\n");
    if (ok) passed++; else failed++;
}

static __attribute__((unused)) void check_eq(const char *what, unsigned long long got, unsigned long long want) {
    check(what, got == want);
    if (got != want) {
        put("       got ");
        put_uint(got);
        put(", expected ");
        put_uint(want);
        put("\r\n");
    }
}

static __attribute__((noreturn)) void finish(const char *name) {
    put(name);
    put(": ");
    put_uint(passed);
    put(" passed, ");
    put_uint(failed);
    put(" failed\r\n");
    ExitProcess(failed);
}

static __attribute__((unused)) int mem_eq(const void *a, const void *b, DWORD n) {
    const unsigned char *x = a, *y = b;
    for (DWORD i = 0; i < n; i++)
        if (x[i] != y[i]) return 0;
    return 1;
}
