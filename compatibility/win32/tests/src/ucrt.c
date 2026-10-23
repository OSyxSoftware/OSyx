/* The Universal C Runtime through the api-ms-win-crt-* contracts (the
 * real UCRT import libraries): heap, strings and ctype, conversions,
 * runtime start-up and exit, threads, environment, file system, time,
 * math, rand_s, locales, low-level I/O, FILE streams (including the
 * buffer protocol msvcp140 uses) and the __stdio_common_* printf family
 * with its option flags. The same PE runs on Windows; the summary is
 * printed by an atexit function after exit() runs the C termination. */
#include "common.h"
#include <stdarg.h>
#include <stdint.h>

/* The declarations below name every function as an import (some are also
 * declared by the MinGW headers windows.h pulls in, without it). */
#pragma clang diagnostic ignored "-Wdll-attribute-on-redeclaration"
#pragma clang diagnostic ignored "-Wincompatible-library-redeclaration"
#pragma clang diagnostic ignored "-Wbuiltin-requires-header"

#define IMP __declspec(dllimport)
typedef struct _iobuf FILE;
typedef void (*pvfv)(void);
typedef int (*pifv)(void);
typedef struct { pvfv *first, *last, *end; } onexit_table;
struct tm_ { int sec, min, hour, mday, mon, year, wday, yday, isdst; };
struct lconv_ { char *decimal_point, *thousands_sep, *grouping; };

/* heap */
IMP void *malloc(size_t); IMP void *calloc(size_t, size_t); IMP void *realloc(void *, size_t); IMP void free(void *);
IMP int _set_new_mode(int); IMP int _callnewh(size_t); IMP size_t _msize(void *);
IMP void *_aligned_malloc(size_t, size_t); IMP void _aligned_free(void *);
/* strings */
IMP size_t strlen(const char *); IMP size_t wcslen(const wchar_t *); IMP int strcmp(const char *, const char *);
IMP int strncmp(const char *, const char *, size_t); IMP int wcscmp(const wchar_t *, const wchar_t *);
IMP int wcsncmp(const wchar_t *, const wchar_t *, size_t); IMP char *strcpy(char *, const char *);
IMP wchar_t *wcscpy(wchar_t *, const wchar_t *); IMP char *strncpy(char *, const char *, size_t);
IMP wchar_t *wcsncpy(wchar_t *, const wchar_t *, size_t); IMP int strcpy_s(char *, size_t, const char *);
IMP int wcscpy_s(wchar_t *, size_t, const wchar_t *); IMP int wcscat_s(wchar_t *, size_t, const wchar_t *);
IMP int wcsncpy_s(wchar_t *, size_t, const wchar_t *, size_t); IMP size_t strnlen(const char *, size_t);
IMP size_t wcsnlen(const wchar_t *, size_t); IMP size_t __strncnt(const char *, size_t); IMP size_t strcspn(const char *, const char *);
IMP wchar_t *wcspbrk(const wchar_t *, const wchar_t *); IMP wchar_t *wcstok_s(wchar_t *, const wchar_t *, wchar_t **);
IMP int _stricmp(const char *, const char *); IMP int _wcsnicmp(const wchar_t *, const wchar_t *, size_t);
IMP wchar_t *_wcsdup(const wchar_t *); IMP int _wcsupr_s(wchar_t *, size_t);
IMP int isupper(int); IMP int islower(int); IMP int isspace(int); IMP int isxdigit(int); IMP int tolower(int);
IMP unsigned short towlower(unsigned short); IMP const unsigned short *__pctype_func(void);
/* convert */
IMP int atoi(const char *); IMP long atol(const char *); IMP long strtol(const char *, char **, int);
IMP unsigned long wcstoul(const wchar_t *, wchar_t **, int); IMP unsigned long long _strtoui64(const char *, char **, int);
IMP double strtod(const char *, char **); IMP float strtof(const char *, char **); IMP double wcstod(const wchar_t *, wchar_t **);
IMP float wcstof(const wchar_t *, wchar_t **); IMP int _ltoa_s(long, char *, size_t, int);
/* runtime */
IMP int *_errno(void); IMP unsigned long *__doserrno(void); IMP void _initterm(pvfv *, pvfv *); IMP int _initterm_e(pifv *, pifv *);
IMP int _initialize_onexit_table(onexit_table *); IMP int _register_onexit_function(onexit_table *, pvfv);
IMP int _execute_onexit_table(onexit_table *); IMP int _crt_atexit(pvfv); IMP void exit(int);
IMP int _configure_narrow_argv(int); IMP int _configure_wide_argv(int); IMP int *__p___argc(void);
IMP char ***__p___argv(void); IMP wchar_t ***__p___wargv(void); IMP int _initialize_narrow_environment(void);
IMP int _initialize_wide_environment(void); IMP wchar_t **_get_initial_wide_environment(void);
IMP uintptr_t _beginthreadex(void *, unsigned, unsigned (__stdcall *)(void *), void *, unsigned, unsigned *);
IMP void _endthreadex(unsigned); IMP int _getpid(void); IMP int _seh_filter_exe(unsigned long, void *);
IMP void _set_app_type(int); IMP void *_set_new_handler(void *);
typedef void (*iph)(const wchar_t *, const wchar_t *, const wchar_t *, unsigned, uintptr_t);
IMP iph _set_invalid_parameter_handler(iph);
/* environment and file system */
IMP char *getenv(const char *); IMP wchar_t *_wgetenv(const wchar_t *); IMP int _putenv(const char *);
IMP char ***__p__environ(void); IMP wchar_t *_wgetcwd(wchar_t *, int); IMP int _wchdir(const wchar_t *);
IMP wchar_t *_wfullpath(wchar_t *, const wchar_t *, size_t); IMP int _wremove(const wchar_t *);
IMP int _wrename(const wchar_t *, const wchar_t *); IMP int _wrmdir(const wchar_t *); IMP int _wmkdir(const wchar_t *);
/* time */
IMP long long _time64(long long *); IMP char *_Getdays(void); IMP char *_Getmonths(void); IMP void *_Gettnames(void);
IMP wchar_t *_W_Getdays(void); IMP size_t _Strftime(char *, size_t, const char *, const struct tm_ *, void *);
IMP size_t _Wcsftime(wchar_t *, size_t, const wchar_t *, const struct tm_ *, void *);
/* math */
IMP double pow(double, double); IMP float powf(float, float); IMP double log10(double); IMP double lgamma(double);
IMP float lgammaf(float); IMP double ldexp(double, int); IMP double frexp(double, int *); IMP double floor(double);
IMP double ceil(double); IMP float ceilf(float); IMP double copysign(double, double); IMP double nan(const char *);
IMP float nanf(const char *); IMP short _dclass(double); IMP short _fdclass(float); IMP int _dsign(double);
IMP int _fdsign(float);
/* utility */
IMP unsigned rand_s(unsigned *);
/* locale */
IMP char *setlocale(int, const char *); IMP wchar_t *_wsetlocale(int, const wchar_t *); IMP int ___mb_cur_max_func(void);
IMP unsigned ___lc_codepage_func(void); IMP unsigned ___lc_collate_cp_func(void); IMP wchar_t **___lc_locale_name_func(void);
IMP int _configthreadlocale(int); IMP struct lconv_ *localeconv(void); IMP void _lock_locales(void); IMP void _unlock_locales(void);
/* low-level I/O and stdio */
IMP int _write(int, const void *, unsigned); IMP intptr_t _get_osfhandle(int); IMP int _set_fmode(int);
IMP FILE *__acrt_iob_func(unsigned); IMP int *__p__commode(void); IMP FILE *_wfopen(const wchar_t *, const wchar_t *);
IMP FILE *_wfsopen(const wchar_t *, const wchar_t *, int); IMP FILE *_fsopen(const char *, const char *, int);
IMP int fclose(FILE *); IMP int fflush(FILE *); IMP int fputc(int, FILE *); IMP int fgetc(FILE *);
IMP int fputs(const char *, FILE *); IMP char *fgets(char *, int, FILE *); IMP size_t fread(void *, size_t, size_t, FILE *);
IMP size_t fwrite(const void *, size_t, size_t, FILE *); IMP int fseek(FILE *, long, int); IMP int _fseeki64(FILE *, long long, int);
IMP long long _ftelli64(FILE *); IMP int fgetpos(FILE *, long long *); IMP int fsetpos(FILE *, const long long *);
IMP int ungetc(int, FILE *); IMP unsigned short fputwc(wchar_t, FILE *); IMP unsigned short fgetwc(FILE *);
IMP unsigned short ungetwc(unsigned short, FILE *); IMP int setvbuf(FILE *, char *, int, size_t); IMP int _fileno(FILE *);
IMP int _get_stream_buffer_pointers(FILE *, char ***, char ***, int **); IMP void _lock_file(FILE *); IMP void _unlock_file(FILE *);
IMP FILE *freopen(const char *, const char *, FILE *);
IMP int __stdio_common_vsprintf(unsigned long long, char *, size_t, const char *, void *, va_list);
IMP int __stdio_common_vsprintf_s(unsigned long long, char *, size_t, const char *, void *, va_list);
IMP int __stdio_common_vsnprintf_s(unsigned long long, char *, size_t, size_t, const char *, void *, va_list);
IMP int __stdio_common_vswprintf(unsigned long long, wchar_t *, size_t, const wchar_t *, void *, va_list);
IMP int __stdio_common_vsnwprintf_s(unsigned long long, wchar_t *, size_t, size_t, const wchar_t *, void *, va_list);
IMP int __stdio_common_vfprintf(unsigned long long, FILE *, const char *, void *, va_list);
IMP int __stdio_common_vfwprintf_s(unsigned long long, FILE *, const wchar_t *, void *, va_list);

#define E (*_errno())
static int ip_calls;
static void on_ip(const wchar_t *a, const wchar_t *b, const wchar_t *c, unsigned d, uintptr_t e) {
    (void)a; (void)b; (void)c; (void)d; (void)e;
    ip_calls++;
}

static int sp(unsigned long long o, char *b, size_t n, const char *f, ...) {
    va_list a; va_start(a, f); int r = __stdio_common_vsprintf(o, b, n, f, NULL, a); va_end(a); return r;
}
static int sps(char *b, size_t n, const char *f, ...) {
    va_list a; va_start(a, f); int r = __stdio_common_vsprintf_s(0, b, n, f, NULL, a); va_end(a); return r;
}
static int snps(char *b, size_t n, size_t c, const char *f, ...) {
    va_list a; va_start(a, f); int r = __stdio_common_vsnprintf_s(0, b, n, c, f, NULL, a); va_end(a); return r;
}
static int swp(unsigned long long o, wchar_t *b, size_t n, const wchar_t *f, ...) {
    va_list a; va_start(a, f); int r = __stdio_common_vswprintf(o, b, n, f, NULL, a); va_end(a); return r;
}
static int snwps(wchar_t *b, size_t n, size_t c, const wchar_t *f, ...) {
    va_list a; va_start(a, f); int r = __stdio_common_vsnwprintf_s(0, b, n, c, f, NULL, a); va_end(a); return r;
}
static int fp(FILE *s, const char *f, ...) {
    va_list a; va_start(a, f); int r = __stdio_common_vfprintf(2, s, f, NULL, a); va_end(a); return r;
}
static int fwp(FILE *s, const wchar_t *f, ...) {
    va_list a; va_start(a, f); int r = __stdio_common_vfwprintf_s(4, s, f, NULL, a); va_end(a); return r;
}

/* Checks a formatted string and the return value. */
static void check_fmt(const char *what, const char *got, int r, const char *want) {
    int ok = strcmp(got, want) == 0 && r == (int)strlen(want);
    check(what, ok);
    if (!ok) { put("       got ["); put(got); put("], expected ["); put(want); put("]\r\n"); }
}

static int close_to(double a, double b, double rel) {
    double d = a - b;
    if (d < 0) d = -d;
    double m = b < 0 ? -b : b;
    return d <= rel * m || d == 0;
}

static int init_count;
static void init_a(void) { init_count += 1; }
static void init_b(void) { init_count += 10; }
static int init_e_ok(void) { init_count += 100; return 0; }
static int init_e_fail(void) { return 7; }
static int init_e_after(void) { init_count += 1000; return 0; }
static char order[8];
static int order_n;
static onexit_table table;
static void ex1(void) { order[order_n++] = '1'; }
static void ex3(void) { order[order_n++] = '3'; }
static void ex2(void) { order[order_n++] = '2'; _register_onexit_function(&table, ex3); }

static volatile LONG thread_value;
static unsigned __stdcall thread_proc(void *arg) {
    *_errno() = 77; /* per-thread errno */
    InterlockedExchange(&thread_value, (LONG)(intptr_t)arg);
    return 42;
}
static unsigned __stdcall thread_end(void *arg) {
    (void)arg;
    _endthreadex(9);
    return 1;
}

static void at_exit_summary(void) {
    check("atexit function runs at exit", 1);
    put("ucrt: "); put_uint(passed); put(" passed, "); put_uint(failed); put(" failed\r\n");
}

static void test_heap(void) {
    char *p = malloc(100);
    check("malloc", p != NULL && _msize(p) == 100);
    memset(p, 'x', 100);
    p = realloc(p, 5000);
    check("realloc keeps the contents", p && p[0] == 'x' && p[99] == 'x' && _msize(p) == 5000);
    free(p);
    int *z = calloc(10, sizeof(int));
    int zero = 1;
    for (int i = 0; i < 10; i++) zero &= z[i] == 0;
    check("calloc zeroes", z && zero);
    check("realloc(p, 0) frees and returns NULL", realloc(z, 0) == NULL);
    E = 0;
    check("calloc overflow fails with ENOMEM", calloc((size_t)-1 / 2, 4) == NULL && E == 12);
    E = 0;
    check("huge malloc fails with ENOMEM", malloc((size_t)-1) == NULL && E == 12);
    void *al = _aligned_malloc(100, 256);
    check("_aligned_malloc honours the alignment", al && ((uintptr_t)al & 255) == 0);
    _aligned_free(al);
    check("_set_new_mode returns the old mode", _set_new_mode(1) == 0 && _set_new_mode(0) == 1);
    ip_calls = 0; E = 0;
    check("_set_new_mode(2) is an invalid parameter", _set_new_mode(2) == -1 && E == 22 && ip_calls == 1);
    check("_callnewh without a handler", _callnewh(10) == 0);
}

static void test_strings(void) {
    char b[16];
    wchar_t w[16];
    check("strlen / wcslen", strlen("hello") == 5 && wcslen(L"hello") == 5);
    check("strcmp orders", strcmp("abc", "abd") < 0 && strcmp("b", "a") > 0 && strcmp("x", "x") == 0);
    check("strncmp / wcsncmp", strncmp("abcx", "abcy", 3) == 0 && wcsncmp(L"ab", L"ac", 2) < 0 && wcscmp(L"z", L"a") > 0);
    check("strcpy / wcscpy", strcmp(strcpy(b, "copy"), "copy") == 0 && wcscmp(wcscpy(w, L"wide"), L"wide") == 0);
    memset(b, 'x', 8);
    strncpy(b, "ab", 5);
    check("strncpy pads with NULs up to n", b[2] == 0 && b[4] == 0 && b[5] == 'x');
    wcsncpy(w, L"abcdef", 3);
    check("wcsncpy copies n units", w[0] == 'a' && w[2] == 'c');
    ip_calls = 0; E = 0;
    check("strcpy_s too small: ERANGE", strcpy_s(b, 3, "abcd") == 34 && b[0] == 0 && E == 34 && ip_calls == 1);
    check("strcpy_s fits", strcpy_s(b, 5, "abcd") == 0 && strcmp(b, "abcd") == 0);
    wchar_t c[6] = L"ab";
    check("wcscat_s appends", wcscat_s(c, 6, L"cd") == 0 && wcscmp(c, L"abcd") == 0);
    ip_calls = 0;
    check("wcscat_s overflow: ERANGE", wcscat_s(c, 6, L"ef") == 34 && c[0] == 0 && ip_calls == 1);
    check("wcscpy_s too small: ERANGE", wcscpy_s(c, 2, L"xy") == 34 && c[0] == 0);
    check("wcsncpy_s count < size", wcsncpy_s(w, 4, L"abcdef", 3) == 0 && wcscmp(w, L"abc") == 0);
    check("wcsncpy_s count >= size: ERANGE", wcsncpy_s(w, 4, L"abcdef", 4) == 34 && w[0] == 0);
    check("wcsncpy_s _TRUNCATE: STRUNCATE", wcsncpy_s(w, 4, L"abcdef", (size_t)-1) == 80 && wcscmp(w, L"abc") == 0);
    check("strnlen / wcsnlen / __strncnt", strnlen("abc", 2) == 2 && wcsnlen(L"abc", 9) == 3 && __strncnt("abc", 10) == 3);
    check("strcspn", strcspn("hello, world", ", ") == 5);
    const wchar_t *s = L"key=value";
    check("wcspbrk", wcspbrk(s, L"=:") == s + 3 && wcspbrk(s, L"#") == NULL);
    wchar_t t[] = L"  a,b;;c ";
    wchar_t *ctx = NULL, *tok;
    int ntok = 0, tok_ok = 1;
    const wchar_t *want[] = {L"a", L"b", L"c"};
    for (tok = wcstok_s(t, L" ,;", &ctx); tok; tok = wcstok_s(NULL, L" ,;", &ctx)) {
        tok_ok &= ntok < 3 && wcscmp(tok, want[ntok]) == 0;
        ntok++;
    }
    check("wcstok_s splits tokens", tok_ok && ntok == 3);
    ip_calls = 0;
    check("wcstok_s without context is invalid", wcstok_s(NULL, L",", NULL) == NULL && ip_calls == 1);
    check("_stricmp folds case", _stricmp("ABC", "abd") == -1 && _stricmp("[", "a") == -6 && _stricmp("HeLLo", "hello") == 0);
    check("_wcsnicmp", _wcsnicmp(L"ABCx", L"abcy", 3) == 0 && _wcsnicmp(L"a", L"B", 1) < 0);
    ip_calls = 0;
    check("_stricmp(NULL) is _NLSCMPERROR", _stricmp(NULL, "a") == 0x7fffffff && ip_calls == 1);
    wchar_t *d = _wcsdup(L"dup");
    check("_wcsdup", d && wcscmp(d, L"dup") == 0);
    free(d);
    wchar_t u[8] = L"aBc1";
    check("_wcsupr_s", _wcsupr_s(u, 8) == 0 && wcscmp(u, L"ABC1") == 0);
    check("_wcsupr_s unterminated within size", _wcsupr_s(u, 3) == 22 && u[0] == 0);
    check("ctype classes", isupper('A') && !isupper('a') && islower('z') && isspace('\t') && isspace(' ') &&
          !isspace('x') && isxdigit('f') && !isxdigit('g'));
    check("ctype of EOF and high bytes", !isspace(-1) && !isupper(0xC4) && !isspace(0x85));
    check("tolower / towlower (C locale)", tolower('Q') == 'q' && tolower(0xC4) == 0xC4 && towlower(L'A') == L'a' && towlower(0xC4) == 0xC4);
    const unsigned short *ct = __pctype_func();
    check("__pctype_func table", ct['A'] == 0x81 && ct['a'] == 0x82 && ct['G'] == 0x1 && ct['0'] == 0x84 && ct[' '] == 0x48 && ct['\t'] == 0x28 &&
          ct['!'] == 0x10 && ct[-1] == 0);
}

static void test_convert(void) {
    char *e;
    wchar_t *we;
    E = 0;
    check("atoi", atoi("  -12abc") == -12);
    E = 0;
    check("atoi overflow saturates with ERANGE", atoi("99999999999") == 0x7fffffff && E == 34);
    check("atol", atol("-99999999999") == (long)0x80000000 && E == 34);
    const char *h = " 0x1f";
    check("strtol base 0 hex", strtol(h, &e, 0) == 31 && e == h + 5);
    h = "077";
    check("strtol base 0 octal", strtol(h, &e, 0) == 63 && e == h + 3);
    h = "0x";
    check("strtol \"0x\" converts nothing", strtol(h, &e, 0) == 0 && e == h);
    h = "+-1";
    check("strtol rejects two signs", strtol(h, &e, 10) == 0 && e == h);
    E = 0;
    check("strtol overflow", strtol("2147483648", &e, 10) == 0x7fffffff && E == 34);
    ip_calls = 0; E = 0;
    h = "10";
    check("strtol base 1 is invalid", strtol(h, &e, 1) == 0 && E == 22 && e == h && ip_calls == 1);
    check("strtol base 36", strtol("zz", NULL, 36) == 1295);
    E = 0;
    check("wcstoul negates", wcstoul(L"-1", &we, 10) == 0xffffffffu && E == 0);
    check("wcstoul overflow", wcstoul(L"4294967296", NULL, 10) == 0xffffffffu && E == 34);
    E = 0;
    check("_strtoui64", _strtoui64("18446744073709551615", NULL, 10) == 0xffffffffffffffffull && E == 0 &&
          _strtoui64("18446744073709551616", NULL, 10) == 0xffffffffffffffffull && E == 34);
    h = "  12.5xyz";
    check("strtod decimal", strtod(h, &e) == 12.5 && e == h + 6);
    check("strtod correctly rounded", strtod("0.1", NULL) == 0.1 && strtod("2.2250738585072014e-308", NULL) == 2.2250738585072014e-308 &&
          strtod("9007199254740993", NULL) == 9007199254740992.0);
    h = "0x1.8p3";
    check("strtod hexadecimal", strtod(h, &e) == 12.0 && e == h + 7);
    E = 0;
    check("strtod overflow: inf, ERANGE", strtod("1e400", NULL) == 1e308 * 10 && E == 34);
    E = 0;
    check("strtod underflow to 0: ERANGE", strtod("1e-400", NULL) == 0 && E == 34);
    E = 0;
    check("strtod subnormal: no ERANGE", strtod("4e-320", NULL) > 0 && E == 0);
    h = "-INFINITY";
    check("strtod infinity", strtod(h, &e) == -(1e308 * 10) && e == h + 9);
    h = "nan(123)";
    double nv = strtod(h, &e);
    check("strtod nan(...)", nv != nv && e == h + 8);
    h = "1e";
    check("strtod leaves a bare exponent", strtod(h, &e) == 1 && e == h + 1);
    h = ".";
    check("strtod \".\" converts nothing", strtod(h, &e) == 0 && e == h);
    h = "0x";
    check("strtod \"0x\" is the zero", strtod(h, &e) == 0 && e == h + 1);
    E = 0;
    check("strtof rounding and overflow", strtof("3.4028235e38", NULL) == 3.4028235e38f && strtof("1e39", NULL) == (float)(1e308 * 10) && E == 34);
    check("wcstod / wcstof", wcstod(L"-2.5e3", &we) == -2500 && *we == 0 && wcstof(L"0.5", NULL) == 0.5f);
    char b[40];
    check("_ltoa_s decimal", _ltoa_s(-12345, b, 7, 10) == 0 && strcmp(b, "-12345") == 0);
    check("_ltoa_s hex", _ltoa_s(255, b, 10, 16) == 0 && strcmp(b, "ff") == 0);
    check("_ltoa_s binary of a negative", _ltoa_s(-255, b, 40, 2) == 0 && strcmp(b, "11111111111111111111111100000001") == 0);
    ip_calls = 0;
    check("_ltoa_s too small: ERANGE", _ltoa_s(-12345, b, 4, 10) == 34 && b[0] == 0 && ip_calls == 1);
    check("_ltoa_s radix 1 is invalid", _ltoa_s(5, b, 10, 1) == 22);
}

static void test_runtime(void) {
    pvfv inits[] = {init_a, NULL, init_b};
    _initterm(inits, inits + 3);
    check("_initterm calls the table, skipping NULL", init_count == 11);
    pifv inits_e[] = {init_e_ok, init_e_fail, init_e_after};
    check("_initterm_e stops at the first failure", _initterm_e(inits_e, inits_e + 3) == 7 && init_count == 111);
    check("_initialize_onexit_table", _initialize_onexit_table(&table) == 0 && _initialize_onexit_table(NULL) == -1);
    check("_register_onexit_function", _register_onexit_function(&table, ex1) == 0 && _register_onexit_function(&table, ex2) == 0);
    check("_execute_onexit_table runs in reverse, with late registrations",
          _execute_onexit_table(&table) == 0 && order_n == 3 && order[0] == '2' && order[1] == '3' && order[2] == '1');
    check("_crt_atexit", _crt_atexit(at_exit_summary) == 0);
    check("_configure_narrow_argv", _configure_narrow_argv(1) == 0);
    int argc = *__p___argc();
    char **argv = *__p___argv();
    check("argv from the command line", argc == 5 && strcmp(argv[1], "one") == 0 && strcmp(argv[2], "two words") == 0 &&
          strcmp(argv[3], "a\\b") == 0 && strcmp(argv[4], "x\"y") == 0 && argv[5] == NULL);
    check("_configure_wide_argv", _configure_wide_argv(1) == 0 && wcscmp((*__p___wargv())[2], L"two words") == 0);
    ip_calls = 0;
    check("_configure_narrow_argv(3) is invalid", _configure_narrow_argv(3) == 22 && ip_calls == 1);
    check("_initialize_narrow_environment", _initialize_narrow_environment() == 0 && *__p__environ() != NULL);
    check("_initialize_wide_environment", _initialize_wide_environment() == 0 && _get_initial_wide_environment() != NULL);
    E = 5;
    check("_errno is the thread's errno", *_errno() == 5);
    check("_getpid", _getpid() == (int)GetCurrentProcessId());
    check("_seh_filter_exe without signal handlers", _seh_filter_exe(0xC0000005, NULL) == 0);
    _set_app_type(1);
    check("_set_new_handler returns the previous handler", _set_new_handler(NULL) == NULL);
    unsigned tid = 0;
    uintptr_t th = _beginthreadex(NULL, 0, thread_proc, (void *)(intptr_t)1234, 0, &tid);
    check("_beginthreadex starts a thread", th != 0 && tid != 0);
    DWORD code = 0;
    check("thread ran and returned its code", WaitForSingleObject((HANDLE)th, 10000) == 0 &&
          GetExitCodeThread((HANDLE)th, &code) && code == 42 && thread_value == 1234);
    check("errno is per thread", *_errno() == 5);
    CloseHandle((HANDLE)th);
    th = _beginthreadex(NULL, 0, thread_end, NULL, 0, NULL);
    check("_endthreadex ends the thread with its code", th != 0 && WaitForSingleObject((HANDLE)th, 10000) == 0 &&
          GetExitCodeThread((HANDLE)th, &code) && code == 9);
    CloseHandle((HANDLE)th);
    ip_calls = 0; E = 0;
    check("_beginthreadex(NULL routine) is invalid", _beginthreadex(NULL, 0, NULL, NULL, 0, NULL) == 0 && E == 22 && ip_calls == 1);
}

static void test_env_fs(void) {
    check("_putenv sets", _putenv("OSYX_UCRT_T=1") == 0 && getenv("osyx_ucrt_t") && strcmp(getenv("osyx_ucrt_t"), "1") == 0);
    char v[8];
    check("_putenv reaches the Win32 environment", GetEnvironmentVariableA("OSYX_UCRT_T", v, 8) == 1 && v[0] == '1');
    check("_wgetenv sees it", _wgetenv(L"OSYX_UCRT_T") && wcscmp(_wgetenv(L"OSYX_UCRT_T"), L"1") == 0);
    check("_putenv with an empty value removes", _putenv("OSYX_UCRT_T=") == 0 && getenv("OSYX_UCRT_T") == NULL &&
          GetEnvironmentVariableA("OSYX_UCRT_T", v, 8) == 0);
    E = 0;
    check("_putenv without '=' fails", _putenv("NOEQUALS") == -1 && E == 22);
    int found = 0;
    for (char **p = *__p__environ(); *p; p++) found |= strncmp(*p, "OSYX_UCRT_T=", 12) == 0;
    check("environ follows _putenv", !found);
    wchar_t cwd[300], cwd2[300], full[300];
    check("_wgetcwd", _wgetcwd(cwd, 300) != NULL && GetCurrentDirectoryW(300, cwd2) && wcscmp(cwd, cwd2) == 0);
    E = 0;
    check("_wgetcwd too small: ERANGE", _wgetcwd(cwd2, 3) == NULL && E == 34);
    wchar_t *m = _wgetcwd(NULL, 0);
    check("_wgetcwd allocates", m && wcscmp(m, cwd) == 0);
    free(m);
    wchar_t tmp[300];
    DWORD n = GetTempPathW(300, tmp);
    tmp[n] = 0;
    wchar_t dir[300], a[300], b[300];
    wcscpy(dir, tmp); wcscpy(dir + wcslen(dir), L"osyx-ucrt-dir");
    _wrmdir(dir);
    check("_wmkdir", _wmkdir(dir) == 0);
    E = 0;
    check("_wmkdir of an existing directory: EEXIST", _wmkdir(dir) == -1 && E == 17);
    check("_wchdir", _wchdir(dir) == 0);
    check("_wfullpath of a relative path", _wfullpath(full, L"x\\..\\y", 300) != NULL &&
          GetFullPathNameW(L"y", 300, cwd2, NULL) && wcscmp(full, cwd2) == 0);
    E = 0;
    check("_wfullpath too small: ERANGE", _wfullpath(full, L"y", 3) == NULL && E == 34);
    wcscpy(a, dir); wcscpy(a + wcslen(a), L"\\f1");
    wcscpy(b, dir); wcscpy(b + wcslen(b), L"\\f2");
    HANDLE f = CreateFileW(a, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    CloseHandle(f);
    check("_wrename", _wrename(a, b) == 0 && GetFileAttributesW(a) == INVALID_FILE_ATTRIBUTES);
    E = 0;
    check("_wrename of a missing file: ENOENT", _wrename(a, b) == -1 && E == 2);
    check("_wremove", _wremove(b) == 0 && GetFileAttributesW(b) == INVALID_FILE_ATTRIBUTES);
    E = 0;
    check("_wremove of a missing file: ENOENT", _wremove(b) == -1 && E == 2);
    check("_wchdir back", _wchdir(cwd) == 0);
    check("_wrmdir", _wrmdir(dir) == 0);
    E = 0;
    check("_wrmdir of a missing directory: ENOENT", _wrmdir(dir) == -1 && E == 2);
}

static void test_time_math(void) {
    long long t = 0, r = _time64(&t);
    check("_time64", r > 1600000000 && r == t);
    char *d = _Getdays();
    check("_Getdays", d && strcmp(d, ":Sun:Sunday:Mon:Monday:Tue:Tuesday:Wed:Wednesday:Thu:Thursday:Fri:Friday:Sat:Saturday") == 0);
    free(d);
    d = _Getmonths();
    check("_Getmonths", d && strncmp(d, ":Jan:January:Feb:February:Mar:March", 35) == 0 && strlen(d) == 134);
    free(d);
    wchar_t *wd = _W_Getdays();
    check("_W_Getdays", wd && wcsncmp(wd, L":Sun:Sunday:Mon", 15) == 0);
    free(wd);
    struct tm_ tm = {5, 4, 13, 9, 3, 121, 5, 98, 0};
    char s[200];
    size_t n = _Strftime(s, sizeof s, "%a %A %b %B %d %H %I %j %m %M %p %S %w %y %Y %%", &tm, NULL);
    check_fmt("_Strftime fields", s, (int)n, "Fri Friday Apr April 09 13 01 099 04 04 PM 05 5 21 2021 %");
    n = _Strftime(s, sizeof s, "%c|%x|%X|%U|%W|%#c|%#x|%Ec|%Oy", &tm, NULL);
    check_fmt("_Strftime %c %x %X %U %W and alternatives", s, (int)n,
              "Fri Apr  9 13:04:05 2021|04/09/21|13:04:05|14|14|Friday, April 09, 2021 13:04:05|Friday, April 09, 2021|Fri Apr  9 13:04:05 2021|21");
    n = _Strftime(s, sizeof s, "%#d|%e|%D|%F|%T|%R|%r|%u|%V|%G|%g|%C|%h", &tm, NULL);
    check_fmt("_Strftime C99 forms", s, (int)n, "9| 9|04/09/21|2021-04-09|13:04:05|13:04|01:04:05 PM|5|14|2021|21|20|Apr");
    E = 0;
    check("_Strftime too small: ERANGE", _Strftime(s, 5, "%Y-%m-%d", &tm, NULL) == 0 && s[0] == 0 && E == 34);
    ip_calls = 0;
    check("_Strftime bad specifier is invalid", _Strftime(s, 50, "%Q", &tm, NULL) == 0 && ip_calls == 1);
    void *names = _Gettnames();
    n = _Strftime(s, sizeof s, "%A %x", &tm, names);
    check_fmt("_Strftime with _Gettnames data", s, (int)n, "Friday 04/09/21");
    free(names);
    wchar_t ws[64];
    n = _Wcsftime(ws, 64, L"%B %d", &tm, NULL);
    check("_Wcsftime", n == 8 && wcscmp(ws, L"April 09") == 0);

    E = 0;
    check("pow exact cases", pow(2, 10) == 1024 && pow(10, -2) == 0.01 && pow(-2, 3) == -8 && pow(10, 22) == 1e22 && E == 0);
    check("pow accuracy", close_to(pow(2, 0.5), 1.4142135623730951, 2.3e-16) && close_to(pow(7, 0.5), 2.6457513110645907, 2.3e-16));
    E = 0;
    double pv = pow(-8, 1.0 / 3);
    check("pow of a negative base: EDOM", pv != pv && E == 33);
    E = 0;
    check("pow overflow: ERANGE", pow(10, 400) == 1e308 * 10 && E == 34);
    check("pow special cases", pow(1, nan("")) == 1 && pow(nan(""), 0) == 1 && pow(-1, 1e308 * 10) == 1 && pow(2, -1074) > 0 &&
          pow(0.5, -(1e308 * 10)) == 1e308 * 10);
    check("powf", powf(2.0f, 3.0f) == 8.0f && close_to(powf(2.0f, 0.5f), 1.41421354f, 1e-7));
    check("log10 exact", log10(1000) == 3 && log10(1e-300) == -300 && close_to(log10(2), 0.3010299956639812, 2.3e-16));
    E = 0;
    check("log10(0): -inf, ERANGE", log10(0) == -(1e308 * 10) && E == 34);
    E = 0;
    double lv = log10(-1);
    check("log10(-1): NaN, EDOM", lv != lv && E == 33);
    check("lgamma values", close_to(lgamma(0.5), 0.57236494292470008, 1e-15) && close_to(lgamma(-0.5), 1.2655121234846454, 1e-15) &&
          close_to(lgamma(3), 0.69314718055994529, 1e-15) && close_to(lgamma(100), 359.13420536957540, 1e-15) &&
          close_to(lgamma(1.0000001), -5.7721558299185064e-08, 1e-12) && close_to(lgamma(-2.5), -0.056243716497674054, 1e-14) &&
          lgamma(1) == 0 && lgamma(2) == 0);
    E = 0;
    check("lgamma poles: inf, ERANGE", lgamma(0) == 1e308 * 10 && E == 34 && lgamma(-3) == 1e308 * 10);
    check("lgammaf", close_to(lgammaf(10.5f), 13.9406252f, 1e-6));
    E = 0;
    check("ldexp", ldexp(3, 4) == 48 && ldexp(3, -1074) > 0 && ldexp(1, -2000) == 0 && E == 0);
    E = 0;
    check("ldexp overflow: ERANGE", ldexp(1, 2000) == 1e308 * 10 && E == 34);
    int ex = 0;
    check("frexp", frexp(-12.0, &ex) == -0.75 && ex == 4);
    check("frexp of a subnormal", frexp(5e-324, &ex) == 0.5 && ex == -1073);
    check("floor / ceil / ceilf", floor(-2.5) == -3 && ceil(-0.5) == 0 && _dsign(ceil(-0.5)) && ceilf(1.2f) == 2.0f);
    check("copysign", copysign(3, -0.0) == -3);
    const char *volatile payload = "0x123"; /* not folded by the compiler */
    double n1 = nan(payload);
    uint64_t bits;
    memcpy(&bits, &n1, 8);
    check_eq("nan ignores the sequence (default quiet NaN)", bits, 0x7ff8000000000000ull);
    float nf = nanf("");
    uint32_t fb;
    memcpy(&fb, &nf, 4);
    check("nanf", fb == 0x7fc00000u);
    check("_dclass", _dclass(0.0) == 0 && _dclass(1.0) == -1 && _dclass(5e-324) == -2 && _dclass(1e308 * 10) == 1 && _dclass(n1) == 2);
    check("_fdclass / _dsign / _fdsign", _fdclass(1.0f) == -1 && _dsign(-0.0) && !_dsign(1.0) && _fdsign(-1.0f));
    unsigned rv = 0, rv2 = 0;
    check("rand_s", rand_s(&rv) == 0 && rand_s(&rv2) == 0 && (rv != rv2 || rv != 0));
    ip_calls = 0;
    check("rand_s(NULL) is invalid", rand_s(NULL) == 22 && ip_calls == 1);
}

static void test_locale(void) {
    check("C locale at start", strcmp(setlocale(0, NULL), "C") == 0 && ___mb_cur_max_func() == 1 &&
          ___lc_codepage_func() == 0 && ___lc_collate_cp_func() == 0 && ___lc_locale_name_func()[2] == NULL);
    check("user default locale exists", setlocale(0, "") != NULL);
    check("C again", strcmp(setlocale(0, "C"), "C") == 0 && ___mb_cur_max_func() == 1);
    check("unknown locale fails", setlocale(0, "xx_BOGUS") == NULL && strcmp(setlocale(0, NULL), "C") == 0);
    check("UTF-8 code page for LC_CTYPE", setlocale(2, ".UTF-8") != NULL && ___mb_cur_max_func() == 4 && ___lc_codepage_func() == 65001);
    char all[200];
    strcpy_s(all, sizeof all, setlocale(0, NULL));
    size_t al = strlen(all);
    check("mixed locale has the composite name", strncmp(all, "LC_COLLATE=C;LC_CTYPE=", 22) == 0 && al > 37 &&
          strcmp(all + al - 37, ";LC_MONETARY=C;LC_NUMERIC=C;LC_TIME=C") == 0);
    check("composite names round-trip", setlocale(0, all) != NULL && ___lc_codepage_func() == 65001);
    check("_wsetlocale back to C", wcscmp(_wsetlocale(0, L"C"), L"C") == 0 && ___mb_cur_max_func() == 1);
    check("_configthreadlocale queries", _configthreadlocale(0) == 2);
    check("_configthreadlocale enables", _configthreadlocale(1) == 2 && _configthreadlocale(0) == 1);
    setlocale(2, ".UTF-8");
    check("per-thread locale changes", ___lc_codepage_func() == 65001);
    check("_configthreadlocale disables", _configthreadlocale(2) == 1 && ___lc_codepage_func() == 0);
    ip_calls = 0; E = 0;
    check("_configthreadlocale(5) is invalid", _configthreadlocale(5) == -1 && E == 22 && ip_calls == 1);
    struct lconv_ *lc = localeconv();
    check("localeconv", lc && strcmp(lc->decimal_point, ".") == 0 && lc->thousands_sep[0] == 0);
    _lock_locales();
    _lock_locales();
    _unlock_locales();
    _unlock_locales();
    check("_lock_locales nests", 1);
}

static void test_printf(void) {
    char b[256];
    int r;
    /* Termination and return values by option. */
    for (int i = 0; i < 16; i++) b[i] = 0x7f;
    r = sp(0, b, 5, "abcdefgh");
    check("vsprintf: truncation returns -2 and terminates", r == -2 && strcmp(b, "abcd") == 0);
    r = sp(0, b, 0, "abcd");
    check("vsprintf: count 0 returns -1", r == -1);
    for (int i = 0; i < 16; i++) b[i] = 0x7f;
    r = sp(1, b, 5, "abcdefgh");
    check("legacy termination: -1, no NUL", r == -1 && mem_eq(b, "abcde\x7f", 6));
    r = sp(1, b, 5, "abcde");
    check("legacy termination: exact fit, no NUL", r == 5 && b[5] == 0x7f);
    r = sp(2, b, 5, "abcdefgh");
    check("standard snprintf: full length, terminated", r == 8 && strcmp(b, "abcd") == 0);
    check("standard snprintf: count 0 measures", sp(2, b, 0, "abcd") == 4 && sp(0, NULL, 0, "abcd") == 4);
    ip_calls = 0;
    r = sps(b, 5, "abcdefgh");
    check("vsprintf_s overflow: ERANGE, empty", r == -1 && b[0] == 0 && E == 34 && ip_calls == 1);
    r = snps(b, 5, (size_t)-1, "abcdefgh");
    check("vsnprintf_s _TRUNCATE", r == -1 && strcmp(b, "abcd") == 0);
    r = snps(b, 5, 3, "abcdefgh");
    check("vsnprintf_s count < size", r == -1 && strcmp(b, "abc") == 0);
    ip_calls = 0;
    r = snps(b, 5, 10, "abcdefgh");
    check("vsnprintf_s count >= size: ERANGE", r == -1 && b[0] == 0 && ip_calls == 1);
    /* Conversions. */
    r = sp(2, b, sizeof b, "%d|%5d|%-5d|%05d|%+d|% d|%x|%X|%#x|%o|%#o|%u|%lld|%hhd|%hd|%zu|%p|%c|%%|%.3d|%.0d|%I64d|%I32x|%#.3x",
           -42, 42, 42, -42, 42, 42, 255, 255, 255, 8, 8, -1, -1LL, 300, 70000, (size_t)7, (void *)0x1234, 'Z', 7, 0, 5LL, 0xab, 0);
    check_fmt("integers", b, r, "-42|   42|42   |-0042|+42| 42|ff|FF|0xff|10|010|4294967295|-1|44|4464|7|0000000000001234|Z|%|007||5|ab|000");
    r = sp(2, b, sizeof b, "%05s|%-05d|%+u|%#5o|%#X|%hhx|%llx|%jd|%td", "ab", 3, 5u, 0u, 0, 0x1ff, -1LL, (long long)-3, (ptrdiff_t)5);
    check_fmt("flags", b, r, "000ab|3    |5|    0|0|ff|ffffffffffffffff|-3|5");
    r = sp(2, b, sizeof b, "%s|%.2s|%5s|%-5s|%S|%ls|%hs|%ws|%C|%lc", "abc", "abc", "ab", "ab", L"wide", L"wl", "hs", L"ws", L'W', L'x');
    check_fmt("strings and characters", b, r, "abc|ab|   ab|ab   |wide|wl|hs|ws|W|x");
    r = sp(2, b, sizeof b, "%s|%.3ls|%5lc", (char *)NULL, L"abcdef", L'x');
    check_fmt("NULL string and precision of wide strings", b, r, "(null)|abc|    x");
    E = 0;
    r = sp(2, b, sizeof b, "%ls", L"\x0444");
    check("%ls outside the C locale: EILSEQ", r == -1 && E == 42);
    r = sp(2, b, sizeof b, "%*d|%-*d|%.*f", 4, 1, 4, 2, 2, 3.14159);
    check_fmt("star width and precision", b, r, "   1|2   |3.14");
    ip_calls = 0; E = 0;
    r = sp(2, b, sizeof b, "%n", &r);
    check("%n is disabled", r == -1 && E == 22 && ip_calls == 1);
    ip_calls = 0;
    r = sp(2, b, sizeof b, "%lz");
    check("bad size modifier is invalid", r == -1 && ip_calls == 1);
    r = sp(2, b, sizeof b, "%y|abc%");
    check_fmt("unknown conversion prints itself", b, r, "y|abc");
    /* Floating point. */
    r = sp(2, b, sizeof b, "%.3e|%E|%G|%10.4f|%-8.2e|%+g|% g|%#g|%#.0f|%g|%g|%g", 1234.5678, 1e-10, 1e20, 3.14159, 2.5, 1.0, 1.0, 1.0,
           2.0, 100000.0, 1000000.0, 0.0001);
    check_fmt("float forms", b, r, "1.235e+03|1.000000E-10|1E+20|    3.1416|2.50e+00|+1| 1|1.00000|2.|100000|1e+06|0.0001");
    r = sp(2, b, sizeof b, "%g|%g|%g|%.3g|%#.3g|%g|%.0g|%G", 0.00001234, 123456.0, 1234567.0, 0.0001, 1.0, 9.9999999, 0.5, 1e-10);
    check_fmt("%g selection", b, r, "1.234e-05|123456|1.23457e+06|0.0001|1.00|10|0.5|1E-10");
    r = sp(2, b, sizeof b, "%.20f|%.40e", 0.1, 1e-300);
    check_fmt("exact digits", b, r, "0.10000000000000000555|1.0000000000000000250590918352087596856961e-300");
    r = sp(2, b, sizeof b, "%.0f|%.0f|%.0f|%.2f|%.1f|%.2e|%.0e", 0.5, 1.5, 2.5, 0.125, 0.25, 1.125, 2.5);
    check_fmt("legacy rounding: half away from zero", b, r, "1|2|3|0.13|0.3|1.13e+00|3e+00");
    r = sp(2 | 32, b, sizeof b, "%.0f|%.0f|%.0f|%.2f|%.1f|%.2e|%.0e", 0.5, 1.5, 2.5, 0.125, 0.25, 1.125, 2.5);
    check_fmt("standard rounding: half to even", b, r, "0|2|2|0.12|0.2|1.12e+00|2e+00");
    r = sp(2 | 16, b, sizeof b, "%e|%g", 1.0, 1e-5);
    check_fmt("legacy three-digit exponents", b, r, "1.000000e+000|1e-005");
    double inf = 1e308 * 10, ind, snan, qn, nq;
    { uint64_t u = 0x7FF0000000000001ULL; memcpy(&snan, &u, 8); u = 0x7FF8000000000001ULL; memcpy(&qn, &u, 8);
      u = 0xFFF8000000000001ULL; memcpy(&nq, &u, 8); u = 0xFFF8000000000000ULL; memcpy(&ind, &u, 8); }
    r = sp(2, b, sizeof b, "%f|%e|%g|%F|%f|%5.1f|%f|%f|%E|%010f|%-8f|%+f", inf, -inf, ind, inf, -ind, inf, snan, qn, snan, inf, -inf, inf);
    check_fmt("infinities and NaNs", b, r, "inf|-inf|-nan(ind)|INF|nan|  inf|nan(snan)|nan|NAN(SNAN)|       inf|-inf    |+inf");
    r = sp(2, b, sizeof b, "%f|%a|%A|%.2a|%a|%08.3a|%012.3a|%f", nq, 0.0, 1.0, 1.0 / 3, 5e-324, 1.0, 1.0, -0.0);
    check_fmt("hex floats", b, r, "-nan|0x0.0000000000000p+0|0X1.0000000000000P+0|0x1.55p-2|0x0.0000000000001p-1022|0x1.000p+0|0x001.000p+0|-0.000000");
    r = sp(2, b, sizeof b, "%e|%e|%.0e|%#.0e|%.15g|%.17g|%g|%f", 1.0, 0.0, 15.0, 15.0, 0.1, 0.1, 123456789.0, 1e20);
    check_fmt("more exponent forms", b, r, "1.000000e+00|0.000000e+00|2e+01|2.e+01|0.1|0.10000000000000001|1.23457e+08|100000000000000000000.000000");
    /* Wide output. */
    wchar_t w[64];
    r = swp(2 | 4, w, 64, L"%s|%S|%c|%C|%d", L"wide", "narrow", L'W', 'n', 5);
    check("legacy wide specifiers", r == 17 && wcscmp(w, L"wide|narrow|W|n|5") == 0);
    r = swp(2, w, 64, L"%s|%ls|%c|%lc", "narrow", L"wide", 'n', L'W');
    check("ISO wide specifiers", r == 15 && wcscmp(w, L"narrow|wide|n|W") == 0);
    r = snwps(w, 4, (size_t)-1, L"%d", 123456);
    check("vsnwprintf_s _TRUNCATE", r == -1 && wcscmp(w, L"123") == 0);
}

static void test_stdio(void) {
    wchar_t tmp[300], path[300];
    DWORD n = GetTempPathW(300, tmp);
    tmp[n] = 0;
    wcscpy(path, tmp);
    wcscpy(path + wcslen(path), L"osyx-ucrt.txt");
    FILE *f = _wfopen(path, L"w");
    check("_wfopen for writing", f != NULL);
    if (!f) {
        check_eq("       errno", (unsigned)*_errno(), 0);
        check_eq("       _doserrno", (unsigned)*__doserrno(), 0);
    }
    check("fputs / fputc", fputs("line1\nline2", f) == 0 && fputc('\n', f) == '\n');
    check("fprintf to a file", fp(f, "%d %s\n", 42, "x") == 5);
    check("_ftelli64 counts CR-LF in text mode", _ftelli64(f) == 20);
    check("fclose", fclose(f) == 0);
    f = _wfopen(path, L"rb");
    char b[64];
    size_t k = fread(b, 1, sizeof b, f);
    check("text mode wrote CR-LF", k == 20 && mem_eq(b, "line1\r\nline2\r\n42 x\r\n", 20));
    fclose(f);
    f = _wfopen(path, L"rt");
    check("fgets reads a line without CR", fgets(b, sizeof b, f) == b && strcmp(b, "line1\n") == 0);
    check("ftell after a translated line", _ftelli64(f) == 7);
    check("fgetc / ungetc", fgetc(f) == 'l' && ungetc('L', f) == 'L' && fgetc(f) == 'L');
    long long pos = 0;
    check("fgetpos", fgetpos(f, &pos) == 0 && pos == 8);
    check("fgets with a small buffer", fgets(b, 4, f) == b && strcmp(b, "ine") == 0);
    check("fsetpos back", fsetpos(f, &pos) == 0 && fgetc(f) == 'i');
    check("fseek to the start", fseek(f, 0, SEEK_SET) == 0 && fgetc(f) == 'l');
    check("fseek SEEK_CUR", fseek(f, 6, SEEK_CUR) == 0 && fgetc(f) == 'l' && _ftelli64(f) == 8);
    check("_fseeki64 SEEK_END", _fseeki64(f, -2, SEEK_END) == 0 && fgetc(f) == '\n' && fgetc(f) == -1);
    check("fgets at end of file", fgets(b, sizeof b, f) == NULL);
    fclose(f);
    /* Binary mode, update mode, wide characters. */
    f = _wfopen(path, L"w+b");
    check("w+b", f && fwrite("abc\ndef", 1, 7, f) == 7);
    check("switch to reading after a seek", fseek(f, 0, SEEK_SET) == 0 && fread(b, 1, 7, f) == 7 && mem_eq(b, "abc\ndef", 7));
    check("binary fputwc writes the code unit", fseek(f, 0, SEEK_END) == 0 && fputwc(0x0444, f) == 0x0444 && _ftelli64(f) == 9);
    check("binary fgetwc", fseek(f, 7, SEEK_SET) == 0 && fgetwc(f) == 0x0444 && fgetwc(f) == 0xFFFF);
    check("binary ungetwc", ungetwc(0x1234, f) == 0x1234 && fgetwc(f) == 0x1234);
    fclose(f);
    f = _wfopen(path, L"w");
    E = 0;
    check("text fputwc outside the C locale: EILSEQ", fputwc(0x0444, f) == 0xFFFF && E == 42);
    check("text fputwc of Latin-1", fputwc(L'A', f) == L'A');
    check("fwprintf_s to a text stream", fwp(f, L"%s=%d", L"k", 7) == 3);
    fclose(f);
    f = _wfopen(path, L"r");
    check("fgetwc in text mode", fgetwc(f) == L'A' && fgetwc(f) == L'k' && ungetwc(L'K', f) == L'K' && fgetwc(f) == L'K');
    fclose(f);
    /* The buffer protocol of msvcp140's basic_filebuf. */
    f = _wfopen(path, L"wb");
    char **base = NULL, **ptr = NULL;
    int *cnt = NULL;
    check("_get_stream_buffer_pointers", _get_stream_buffer_pointers(f, &base, &ptr, &cnt) == 0 && base && ptr && cnt);
    check("first put overflows into the buffer", fputc('a', f) == 'a' && *cnt > 0 && *ptr == *base + 1);
    for (int i = 0; i < 3; i++) {
        *(*ptr)++ = (char)('b' + i);
        --*cnt;
    }
    check("bytes stored through the pointers are written", fflush(f) == 0 && _ftelli64(f) == 4);
    fclose(f);
    f = _wfopen(path, L"rb");
    _get_stream_buffer_pointers(f, &base, &ptr, &cnt);
    check("underflow fills the buffer", fgetc(f) == 'a' && *cnt == 3 && **ptr == 'b');
    ++*ptr;
    --*cnt;
    check("reads through the pointers advance the stream", fgetc(f) == 'c' && _ftelli64(f) == 3);
    fclose(f);
    /* Buffering, locking, sharing, modes. */
    f = _wfopen(path, L"w");
    char vb[64];
    check("setvbuf with a user buffer", setvbuf(f, vb, 0, sizeof vb) == 0);
    fputs("buffered", f);
    HANDLE h = CreateFileW(path, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, OPEN_EXISTING, 0, NULL);
    DWORD rd = 0;
    check("output stays in the buffer until flushed", ReadFile(h, b, 64, &rd, NULL) && rd == 0);
    fflush(f);
    SetFilePointer(h, 0, NULL, FILE_BEGIN);
    check("fflush writes it", ReadFile(h, b, 64, &rd, NULL) && rd == 8);
    CloseHandle(h);
    ip_calls = 0;
    check("setvbuf bad mode is invalid", setvbuf(f, NULL, 3, 10) == -1 && ip_calls == 1);
    _lock_file(f);
    _lock_file(f);
    _unlock_file(f);
    _unlock_file(f);
    check("_fileno / _get_osfhandle", _fileno(f) > 2 && _get_osfhandle(_fileno(f)) != -1);
    fclose(f);
    FILE *g = _wfsopen(path, L"r", 0x10 /* _SH_DENYRW */);
    check("_wfsopen deny-all", g != NULL);
    E = 0;
    check("a second open is refused: EACCES", _wfopen(path, L"r") == NULL && E == 13);
    fclose(g);
    ip_calls = 0; E = 0;
    check("invalid mode is an invalid parameter", _wfopen(path, L"q") == NULL && E == 22 && ip_calls == 1);
    E = 0;
    check("missing file: ENOENT", _wfopen(L"C:\\no-such-dir-osyx\\x.txt", L"r") == NULL && E == 2);
    check("_set_fmode binary", _set_fmode(0x8000) == 0);
    f = _wfopen(path, L"w");
    fputs("a\nb", f);
    fclose(f);
    _set_fmode(0x4000);
    f = _wfopen(path, L"rb");
    check("_fmode binary: no translation", fread(b, 1, 10, f) == 3);
    fclose(f);
    char np[300];
    WideCharToMultiByte(CP_UTF8, 0, path, -1, np, 300, NULL, NULL);
    f = _fsopen(np, "a", 0x40);
    check("append mode", f && fputs("+", f) == 0 && fclose(f) == 0);
    f = _fsopen(np, "r", 0x40);
    check("appended at the end", fread(b, 1, 10, f) == 4 && b[3] == '+');
    FILE *re = freopen(np, "rb", f);
    check("freopen", re == f && fread(b, 1, 10, f) == 4);
    fclose(f);
    _wremove(path);
    FILE *out = __acrt_iob_func(1);
    check("__acrt_iob_func gives the standard streams", out != NULL && _fileno(out) == 1 && _fileno(__acrt_iob_func(2)) == 2);
    check("__p__commode", __p__commode() != NULL && *__p__commode() == 0);
    fflush(out);
    check("printf to stdout", fp(out, "ucrt: %s\n", "printf to stdout") == 23 && fflush(out) == 0);
    check("_write to fd 1", _write(1, "ucrt: _write to fd 1\n", 21) == 21);
}

void __stdcall start(void) {
    _set_invalid_parameter_handler(on_ip);
    test_heap();
    test_strings();
    test_convert();
    test_runtime();
    test_env_fs();
    test_time_math();
    test_locale();
    test_printf();
    test_stdio();
    if (passed < 0) finish("ucrt"); /* the summary comes from the atexit function */
    exit(failed);
}
