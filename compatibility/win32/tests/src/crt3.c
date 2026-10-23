/* Independent PE fixture, run against the Windows UCRT and OSyx. Calls
 * __stdio_common_* directly, so the compiler's stdio wrappers cannot hide
 * ABI, bounded-input, secure-size or wide-format option differences. */
#include "common.h"
#include <stdarg.h>
#include <stdint.h>
#pragma clang diagnostic ignored "-Wdll-attribute-on-redeclaration"
#define IMP __declspec(dllimport)
typedef struct _iobuf FILE;
typedef void (*iph)(const wchar_t *, const wchar_t *, const wchar_t *, unsigned, uintptr_t);
IMP iph _set_invalid_parameter_handler(iph);
IMP int *_errno(void);
IMP int __stdio_common_vsscanf(uint64_t, const char *, size_t, const char *, void *, va_list);
IMP int __stdio_common_vswscanf(uint64_t, const wchar_t *, size_t, const wchar_t *, void *, va_list);
IMP int __stdio_common_vfscanf(uint64_t, FILE *, const char *, void *, va_list);
IMP int __stdio_common_vfwscanf(uint64_t, FILE *, const wchar_t *, void *, va_list);
IMP int strcmp(const char *, const char *);
IMP int wcscmp(const wchar_t *, const wchar_t *);
IMP char *strerror(int);
IMP int strerror_s(char *, size_t, int);
IMP wchar_t *_wcserror(int);
IMP int _wcserror_s(wchar_t *, size_t, int);
IMP void perror(const char *);
IMP void _wperror(const wchar_t *);
IMP unsigned _control87(unsigned, unsigned);
IMP unsigned _controlfp(unsigned, unsigned);
IMP int _controlfp_s(unsigned *, unsigned, unsigned);
IMP int fegetround(void);
IMP int fesetround(int);
IMP double rint(double);
IMP void _fpreset(void);
IMP FILE *fopen(const char *, const char *);
IMP int fclose(FILE *);
IMP size_t fwrite(const void *, size_t, size_t, FILE *);
IMP int fgetc(FILE *);
IMP unsigned short fgetwc(FILE *);
IMP int fseek(FILE *, long, int);
IMP int remove(const char *);
IMP char *setlocale(int, const char *);
IMP int _putenv(const char *);
struct imaxdiv_ { long long quot, rem; };
IMP struct imaxdiv_ imaxdiv(long long, long long);
IMP void _tzset(void);
IMP int _get_timezone(long *);
IMP int _get_daylight(int *);
IMP int _get_dstbias(long *);
struct tm_ { int sec, min, hour, mday, mon, year, wday, yday, isdst; };
IMP int _localtime64_s(struct tm_ *, const long long *);
IMP int _gmtime64_s(struct tm_ *, const long long *);
IMP long long _mktime64(struct tm_ *);
IMP long long _mkgmtime64(struct tm_ *);
struct timeb_ { long long time; unsigned short millitm; short timezone, dstflag; };
IMP void _ftime64(struct timeb_ *);

static int invalid_calls;
static void invalid(const wchar_t *a, const wchar_t *b, const wchar_t *c, unsigned d, uintptr_t e) {
    (void)a; (void)b; (void)c; (void)d; (void)e; invalid_calls++;
}
static int scan(uint64_t o, const char *s, size_t n, const char *fmt, ...) {
    va_list a; va_start(a, fmt);
    int r = __stdio_common_vsscanf(o, s, n, fmt, NULL, a);
    va_end(a); return r;
}
static int wscan(uint64_t o, const wchar_t *s, size_t n, const wchar_t *fmt, ...) {
    va_list a; va_start(a, fmt);
    int r = __stdio_common_vswscanf(o, s, n, fmt, NULL, a);
    va_end(a); return r;
}
static int stream(FILE *s, const char *fmt, ...) {
    va_list a; va_start(a, fmt);
    int r = __stdio_common_vfscanf(0, s, fmt, NULL, a);
    va_end(a); return r;
}
static int wstream(FILE *s, const wchar_t *fmt, ...) {
    va_list a; va_start(a, fmt);
    int r = __stdio_common_vfwscanf(2, s, fmt, NULL, a);
    va_end(a); return r;
}
#define ALL ((size_t)-1)
#define E (*_errno())

static void scans(void) {
    int i = 0, j = 0, n = -1;
    unsigned u = 0;
    long l = 0;
    long long ll = 0;
    short h = 0;
    signed char b = 0;
    char s[32] = {0}, c[4] = {'X','X','X','X'};
    wchar_t w[32] = {0};
    double d = 0;
    float f = 0;
    check("scanf integer bases and count", scan(0, " -42 0x2a 075 rest", ALL, "%d %i %o%n", &i, &j, &u, &n) == 3 && i == -42 && j == 42 && u == 61 && n == 13);
    check("scanf Windows integer destination sizes", scan(0, "-2 -32000 2147483647 -9223372036854775807", ALL, "%hhd %hd %ld %I64d", &b, &h, &l, &ll) == 4 && b == -2 && h == -32000 && l == 2147483647 && ll == -9223372036854775807LL);
    check("scanf suppression consumes no argument", scan(0, "123 456", ALL, "%*d %d", &i) == 1 && i == 456);
    check("scanf width and unmatched literal", scan(0, "12345!", ALL, "%3d%2d?", &i, &j) == 2 && i == 123 && j == 45);
    char bounded[] = {'1','2','3'};
    check("scanf bounded input needs no terminator", scan(0, bounded, 3, "%d%n", &i, &n) == 1 && i == 123 && n == 3);
    check("scanf empty input returns EOF", scan(0, "", ALL, "%d", &i) == -1);
    check("scanf zero input length returns EOF", scan(0, "42", 0, "%d", &i) == -1);
    check("scanf mismatch returns zero", scan(0, "abc", ALL, "%d", &i) == 0);
    check_eq("scanf EOF after a suppressed conversion", scan(0, "1", ALL, "%*d%d", &i), -1);
    check("scanf percent n counts without an assignment", scan(0, "12", ALL, "%d%n", &i, &n) == 1 && n == 2);
    check("scanf strings and literal percent", scan(0, "  abc%42", ALL, "%3s%%%d", s, &i) == 2 && strcmp(s, "abc") == 0 && i == 42);
    check("scanf c includes whitespace without a terminator", scan(0, " aZ", ALL, "%2c", c) == 1 && c[0] == ' ' && c[1] == 'a' && c[2] == 'X');
    check("scanf scanset ranges", scan(0, "abcXYZ", ALL, "%[a-c]%[X-Z]", s, s+8) == 2 && strcmp(s,"abc") == 0 && strcmp(s+8,"XYZ") == 0);
    check("scanf inverse scanset and initial bracket", scan(0, "]aa,rest", ALL, "%[]a]%*c%[^!]", s, s+8) == 2 && strcmp(s,"]aa") == 0 && strcmp(s+8,"rest") == 0);
    check("scanf floating destinations and hexadecimal input", scan(0, "1.25 -0x1.8p+2", ALL, "%f %lf", &f, &d) == 2 && f == 1.25f && d == -6.0);
    check("scanf infinity and nan", scan(0, "-infinity nan", ALL, "%lf %f", &d, &f) == 2 && d < -1e308 && f != f);
    check("scanf exponent width", scan(0, "2.5e2!", 5, "%lf%n", &d, &n) == 1 && d == 250 && n == 5);
    d = 99;
    check("scanf incomplete exponent at EOF", scan(0, "1e+", ALL, "%lf", &d) == -1 && d == 99);
    check("scanf failed conversion before EOF", scan(0, "1e+!", ALL, "%lf", &d) == 0 && d == 99);
    void *ptr = NULL;
    check("scanf pointer width is 64 bits", scan(0, "123456789ABC", ALL, "%p", &ptr) == 1 && (uintptr_t)ptr == 0x123456789abcULL);
    check("secure scanf string capacity", scan(1, "hello 7", ALL, "%5s%d", s, 6u, &i) == 2 && strcmp(s,"hello") == 0 && i == 7);
    s[0] = 'X'; E = 0;
    check("secure scanf rejects a small buffer", scan(1, "hello", ALL, "%s", s, 3u) == 0 && s[0] == 0);
    c[2] = 'X';
    check("secure scanf c has no terminator", scan(1, "ab", ALL, "%2c", c, 2u) == 1 && c[0] == 'a' && c[1] == 'b' && c[2] == 'X');
    check("wide scanf standard s destination is narrow", wscan(0, L"hello 12", ALL, L"%s %d", s, &i) == 2 && strcmp(s,"hello") == 0 && i == 12);
    check("wide scanf legacy s destination is wide", wscan(2, L"hello", ALL, L"%s", w) == 1 && wcscmp(w,L"hello") == 0);
    check("wide scanf explicit narrow and wide", wscan(0, L"abc def", ALL, L"%hs %ls", s, w) == 2 && strcmp(s,"abc") == 0 && wcscmp(w,L"def") == 0);
    check("narrow scanf explicit wide destination", scan(0, "abc", ALL, "%ls", w) == 1 && wcscmp(w,L"abc") == 0);
    setlocale(2, ".UTF-8");
    char osyx[8];
    if (GetEnvironmentVariableA("OSYX", osyx, sizeof osyx)) {
        /* The host's UCRT scanf sign-extends a leading UTF8 byte in this
         * case. OSyx uses its shared UTF8 decoder; test that separately. */
        check("OSyx scanf decodes UTF8 into UTF16", scan(0, "\xc5\xbc", ALL, "%ls%n", w, &n) == 1 && w[0] == 0x17c && w[1] == 0 && n == 2);
        check("OSyx scanf wide field width counts whole characters", scan(0, "\xc5\xbc!", ALL, "%1ls%n", w, &n) == 1 && w[0] == 0x17c && w[1] == 0 && n == 2);
    }
    check("wide scanf UTF16 to UTF8", wscan(0, L"\x17c", ALL, L"%s", s) == 1 && strcmp(s,"\xc5\xbc") == 0);
    setlocale(2, "C");
    invalid_calls = 0; E = 0;
    check("scanf null format invokes invalid parameter handler", scan(0, "a", ALL, NULL) == -1 && invalid_calls == 1 && E == 22);
    invalid_calls = 0; E = 0;
    check("scanf null destination invokes invalid parameter handler", scan(0, "1", ALL, "%d", NULL) == -1 && invalid_calls == 1 && E == 22);
    FILE *file = fopen("crt3-input.tmp", "w+b");
    check("scanf stream fixture opens", file != NULL);
    if (file) {
        fwrite("12!34", 1, 5, file); fseek(file, 0, 0);
        check("fscanf leaves the next input byte unread", stream(file, "%d", &i) == 1 && i == 12 && fgetc(file) == '!');
        check("fscanf continues from the current position", stream(file, "%d", &i) == 1 && i == 34 && stream(file, "%d", &i) == -1);
        fclose(file); remove("crt3-input.tmp");
    }
    file = fopen("crt3-wide.tmp", "w+b");
    if (file) {
        const wchar_t data[] = L"42!";
        fwrite(data, 2, 3, file); fseek(file, 0, 0);
        check("fwscanf reads and restores UTF16 lookahead", wstream(file, L"%d", &i) == 1 && i == 42 && fgetwc(file) == '!');
        fclose(file); remove("crt3-wide.tmp");
    } else { check("wide scanf fixture opens", 0); }
}

static DWORD WINAPI rounding_thread(void *p) {
    (void)p;
    fesetround(0x200); Sleep(20);
    return fegetround() == 0x200 && rint(1.25) == 2.0 ? 0 : 1;
}
static void controls(void) {
    struct imaxdiv_ divided = imaxdiv(-17, 5);
    check("imaxdiv returns its 128-bit structure using the Windows ABI", divided.quot == -3 && divided.rem == -2);
    _fpreset();
    unsigned old = _control87(0,0), word = 0;
    check("control87 reads Windows masks", (old & 0x8001f) == 0x8001f && fegetround() == 0);
    check("controlfp_s sets downward rounding", _controlfp_s(&word,0x100,0x300) == 0 && (word & 0x300) == 0x100 && fegetround() == 0x100 && rint(1.75) == 1.0);
    check("fesetround changes hardware rounding", fesetround(0x200) == 0 && ( _controlfp(0,0) & 0x300) == 0x200 && rint(1.25) == 2.0);
    check("fesetround rejects invalid mode", fesetround(1) != 0 && fegetround() == 0x200);
    fesetround(0x100);
    HANDLE t = CreateThread(NULL, 0, rounding_thread, NULL, 0, NULL);
    if (t) {
        WaitForSingleObject(t, INFINITE); DWORD code = 99; GetExitCodeThread(t, &code); CloseHandle(t);
        check("floating point rounding belongs to each thread", code == 0 && fegetround() == 0x100 && rint(1.75) == 1.0);
    } else { check("rounding thread created", 0); }
    _control87(0x80000, 0x80000);
    check("controlfp preserves denormal exception mask", (_controlfp(0,0x80000) & 0x80000) != 0);
    check("control87 can change denormal exception mask", (_control87(0,0x80000) & 0x80000) == 0);
    _control87(0x80000, 0x80000);
    invalid_calls = 0; E = 0;
    check("controlfp_s ignores a zero precision request", _controlfp_s(&word,0,0x30000) == 0);
    check("controlfp_s ignores x87 precision on x64", _controlfp_s(&word,0x20000,0x30000) == 0 && invalid_calls == 0);
    int fp_result = _controlfp_s(&word,0x40000000,0x40000000);
    check_eq("controlfp_s unknown-bit result", fp_result, 22);
    check_eq("controlfp_s unknown-bit errno", E, 22);
    check_eq("controlfp_s unknown-bit handler", invalid_calls, 1);
    _fpreset();
    check("fpreset restores round to nearest", fegetround() == 0 && rint(2.5) == 2.0);
}

static void diagnostics(void) {
    char b[64]; wchar_t w[64];
    check("strerror returns Windows errno text", strcmp(strerror(2),"No such file or directory") == 0);
    check("strerror unknown error", strcmp(strerror(-999),"Unknown error") == 0);
    check("strerror_s copies message", strerror_s(b,sizeof b,22) == 0 && strcmp(b,"Invalid argument") == 0);
    check("strerror_s truncates with NUL", strerror_s(b,4,22) == 0 && strcmp(b,"Inv") == 0);
    check("wcserror returns UTF16", wcscmp(_wcserror(13),L"Permission denied") == 0);
    check("wcserror_s copies message", _wcserror_s(w,64,9) == 0 && wcscmp(w,L"Bad file descriptor") == 0);
    invalid_calls = 0; E = 0;
    check("strerror_s validates output", strerror_s(NULL,0,22) == 22 && E == 22 && invalid_calls == 1);
    E = 2; perror("crt3 expected diagnostic"); _wperror(L"crt3 wide diagnostic");
    check("perror preserves errno", E == 2);
}

static void times(void) {
    _putenv("TZ=UTC0"); _tzset();
    long west = 1, bias = 0; int daylight = -1;
    check("tzset UTC globals", _get_timezone(&west) == 0 && west == 0 && _get_daylight(&daylight) == 0 && daylight == 0);
    struct tm_ v = {0,0,12,15,0,126,0,0,0};
    long long jan = _mkgmtime64(&v);
    check("mkgmtime UTC calendar conversion", jan == 1768478400LL && v.wday == 4 && v.yday == 14);
    _putenv("TZ=PST8PDT"); _tzset();
    check("tzset standard and daylight offsets", _get_timezone(&west) == 0 && west == 28800 && _get_daylight(&daylight) == 0 && daylight == 1 && _get_dstbias(&bias) == 0 && bias == -3600);
    check("localtime applies winter standard offset", _localtime64_s(&v,&jan) == 0 && v.hour == 4 && v.isdst == 0);
    check("mktime local winter round trip", _mktime64(&v) == jan && v.hour == 4);
    v = (struct tm_){0,0,12,15,6,126,0,0,0};
    long long jul = _mkgmtime64(&v);
    check("localtime applies summer daylight offset", _localtime64_s(&v,&jul) == 0 && v.hour == 5 && v.isdst == 1);
    v.isdst = -1;
    check("mktime detects daylight offset", _mktime64(&v) == jul && v.hour == 5 && v.isdst == 1);
    long long before = 1772963999LL, after = 1772964000LL;
    check("localtime before US spring transition", _localtime64_s(&v,&before) == 0 && v.hour == 1 && v.min == 59 && v.isdst == 0);
    check("localtime after US spring transition", _localtime64_s(&v,&after) == 0 && v.hour == 3 && v.min == 0 && v.isdst == 1);
    _putenv("TZ=XXX-5:30"); _tzset();
    check("tzset supports fractional-hour offsets", _get_timezone(&west) == 0 && west == -19800 && _localtime64_s(&v,&jan) == 0 && v.hour == 17 && v.min == 30);
    long long bad = -1; invalid_calls = 0; E = 0;
    check("localtime rejects negative time and clears output", _localtime64_s(&v,&bad) == 22 && v.year == -1 && E == 22);
    _putenv("TZ=UTC0"); _tzset();
    struct timeb_ now;
    _ftime64(&now);
    check("ftime64 time and milliseconds", now.time > 1600000000LL && now.millitm < 1000);
    check("ftime64 uses the CRT timezone", now.timezone == 0);
    _putenv("TZ=");
}

void __stdcall start(void) {
    _set_invalid_parameter_handler(invalid);
    scans(); controls(); diagnostics(); times();
    finish("crt3");
}
