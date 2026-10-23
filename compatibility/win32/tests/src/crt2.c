/* The C runtime functions xul.dll, nss3 and gkcodecs import beyond ucrt.c:
 * the rest of the math contract, checked against the UCRT's own results
 * (within 1 ulp), and special cases. Links the real UCRT import library. */
#include "common.h"
#include <math.h>
#include <errno.h>
#include "crtmath_cases.h"

static double d(unsigned long long b) {
    double x;
    memcpy(&x, &b, 8);
    return x;
}

static unsigned long long bits(double x) {
    unsigned long long b;
    memcpy(&b, &x, 8);
    return b;
}

/* Distance in units in the last place (doubles of the same sign). */
static unsigned long long ulps(double a, double b) {
    if (a == b) return 0;
    if (a != a && b != b) return 0;
    long long x = (long long)bits(a), y = (long long)bits(b);
    if ((x < 0) != (y < 0)) return 1ULL << 62;
    return x > y ? (unsigned long long)(x - y) : (unsigned long long)(y - x);
}

static double call(int f, double a, double b, double c) {
    double i;
    switch (f) {
    case F_acos: return acos(a);
    case F_acosh: return acosh(a);
    case F_asin: return asin(a);
    case F_asinh: return asinh(a);
    case F_atan: return atan(a);
    case F_atan2: return atan2(a, b);
    case F_atan2f: return atan2f((float)a, (float)b);
    case F_atanh: return atanh(a);
    case F_cbrt: return cbrt(a);
    case F_cos: return cos(a);
    case F_cosh: return cosh(a);
    case F_exp2: return exp2(a);
    case F_expf: return expf((float)a);
    case F_expm1: return expm1(a);
    case F_fma: return fma(a, b, c);
    case F_hypot: return hypot(a, b);
    case F_log1p: return log1p(a);
    case F_log2: return log2(a);
    case F_lround: return (double)llround(a);
    case F_modf: return modf(a, &i);
    case F_nextafter: return nextafter(a, b);
    case F_remainder: return remainder(a, b);
    case F_rint: return rint(a);
    case F_sin: return sin(a);
    case F_sinf: return sinf((float)a);
    case F_sinh: return sinh(a);
    case F_sqrtf: return sqrtf((float)a);
    case F_tan: return tan(a);
    case F_tanh: return tanh(a);
    }
    return 0;
}

static const char *const names[] = {"acos", "acosh", "asin", "asinh", "atan", "atan2", "atan2f", "atanh", "cbrt", "cos",
                                    "cosh", "exp2", "expf", "expm1", "fma", "hypot", "log1p", "log2", "lround", "modf",
                                    "nextafter", "remainder", "rint", "sin", "sinf", "sinh", "sqrtf", "tan", "tanh"};

void __stdcall start(void) {
    int bad = 0;
    for (unsigned i = 0; i < sizeof math_cases / sizeof math_cases[0]; i++) {
        double got = call(math_cases[i].f, d(math_cases[i].a), d(math_cases[i].b), d(math_cases[i].c));
        double want = d(math_cases[i].want);
        /* The float functions are compared at float precision. */
        int is_float = math_cases[i].f == F_sinf || math_cases[i].f == F_sqrtf || math_cases[i].f == F_expf || math_cases[i].f == F_atan2f;
        unsigned long long u = is_float ? ulps((double)(float)got, (double)(float)want) >> 29 : ulps(got, want);
        if (u > 1) {
            bad++;
            put("       ");
            put(names[math_cases[i].f]);
            put(" case ");
            put_uint(i);
            put(" is off by ");
            put_uint(u);
            put(" ulps\r\n");
        }
    }
    check("math functions agree with the UCRT within 1 ulp", bad == 0);
    check("sin of a huge argument is reduced exactly", sin(1e22) == d(0xbfeb45b6a2fd7f2aULL) || ulps(sin(1e22), -0.8522008497671888) <= 1);
    check("fma rounds once", fma(1.0 + 0x1p-52, 1.0 - 0x1p-53, -1.0) == 0x1p-53 - 0x1p-105);
    check("cbrt of a cube is exact", cbrt(-27.0) == -3.0 && cbrt(64.0) == 4.0 && cbrt(0x1p-999) == 0x1p-333);
    check("hypot avoids overflow", hypot(1e300, 1e300) == 1e300 * 1.4142135623730951);
    check("log2 and exp2 are exact on powers of two", log2(0x1p-1000) == -1000.0 && exp2(-1000.0) == 0x1p-1000);
    check("remainder rounds the quotient to even", remainder(5.0, 2.0) == 1.0 && remainder(7.0, 2.0) == -1.0);
    check("rint rounds half to even", rint(2.5) == 2.0 && rint(-3.5) == -4.0 && nearbyint(0.5) == 0.0);
    check("llrintf uses the floating argument ABI", llrintf(2.5f) == 2 && llrintf(-3.5f) == -4);
    check("lround and llround round half away", lround(-2.5) == -3 && llround(2.5) == 3 && lroundf(0.5f) == 1);
    double ip = 0;
    check("modf splits an infinity", modf(-INFINITY, &ip) == 0.0 && ip == -INFINITY);
    errno = 0;
    check("asin outside [-1, 1] is a domain error", isnan(asin(2.0)) && errno == EDOM);
    errno = 0;
    check("atanh(1) is a pole", atanh(1.0) == INFINITY && errno == ERANGE);
    errno = 0;
    check("sinh overflow sets ERANGE", sinh(1000.0) == INFINITY && errno == ERANGE);
    check("sinh and cosh remain finite near overflow", ulps(sinh(710.0), 1.1169973830808557e308) <= 1 && ulps(cosh(710.0), 1.1169973830808557e308) <= 1);
    check("tanh saturates", tanh(30.0) == 1.0 && tanh(-30.0) == -1.0);
    check("signed zeros survive", 1.0 / sin(-0.0) < 0 && 1.0 / atan(-0.0) < 0 && 1.0 / expm1(-0.0) < 0 && 1.0 / log1p(-0.0) < 0);
    check("nextafter steps to the smallest subnormal", nextafter(0.0, -1.0) == -0x1p-1074);
    check("scalbn", scalbn(1.0, 1023) == 0x1p1023 && scalbnf(1.5f, 3) == 12.0f);
    check("float variants", fabsf(sinf(0.0f)) == 0.0f && cosf(0.0f) == 1.0f && roundf(-2.5f) == -3.0f && fmodf(7.0f, 4.0f) == 3.0f);
    finish("crt2");
}
