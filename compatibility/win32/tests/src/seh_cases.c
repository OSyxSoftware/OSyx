/* Structured exception handling cases, compiled for the MSVC ABI
 * (clang --target=x86_64-pc-windows-msvc) so that __try/__except/__finally
 * produce real scope tables for __C_specific_handler. No headers: the
 * mingw ones do not build for that target. Linked into seh.exe. */
typedef unsigned long DWORD;
typedef unsigned long long U64;
typedef struct {
    DWORD code, flags;
    void *nested, *address;
    DWORD count;
    U64 info[15];
} RECORD;
typedef struct __declspec(align(16)) { unsigned char bytes[1232]; } CTX;

__declspec(dllimport) void __stdcall RaiseException(DWORD, DWORD, DWORD, const U64 *);
__declspec(dllimport) void __stdcall RtlCaptureContext(CTX *);
__declspec(dllimport) void __stdcall RtlRestoreContext(CTX *, RECORD *);

#define CODE 0xE0001234u
#define RIP(c) (*(U64 *)((c)->bytes + 0xF8))

static volatile int order[8], n_order;
static void mark(int v) { order[n_order++] = v; }

__declspec(noinline) void seh_fault(volatile int *p) { *p = 1; }
__declspec(noinline) void seh_raise(DWORD code, DWORD flags) {
    U64 arg = 77;
    RaiseException(code, flags, 1, &arg);
}

/* A hardware access violation in a callee, filtered by code. */
int seh_access_violation(void) {
    volatile int r = 0;
    __try { seh_fault((volatile int *)0x10); r = 1; }
    __except (_exception_code() == 0xC0000005 ? 1 : 0) { r = 2; }
    return r;
}

/* A software exception: the __except block sees its code. */
DWORD seh_software_code(void) {
    volatile DWORD code = 0;
    __try { seh_raise(CODE, 0); }
    __except (1) { code = _exception_code(); }
    return code;
}

/* Inner __finally runs during the unwind to the outer __except. */
int seh_finally_order(void) {
    n_order = 0;
    __try {
        __try { mark(1); seh_raise(CODE, 0); mark(99); }
        __finally { mark(2); }
    }
    __except (mark(0), 1) { mark(3); }
    return order[0] * 1000 + order[1] * 100 + order[2] * 10 + order[3] + n_order * 10000;
}

/* A filter that declines; the outer handler takes it. */
static int seen_inner;
static int decline(void) { seen_inner = 1; return 0; }
int seh_continue_search(void) {
    volatile int r = 0;
    seen_inner = 0;
    __try {
        __try { seh_raise(CODE, 0); }
        __except (decline()) { r = 99; }
    }
    __except (1) { r = 5; }
    return r + seen_inner * 10;
}

/* EXCEPTION_CONTINUE_EXECUTION resumes after RaiseException. */
int seh_continue_execution(void) {
    volatile int r = 0;
    __try { seh_raise(CODE, 0); r = 7; }
    __except (-1) { r = 99; }
    return r;
}

/* A normal exit from __try also runs __finally (no unwind). */
int seh_finally_normal(void) {
    volatile int r = 0;
    __try { r = 1; }
    __finally { r += 10; }
    return r;
}

/* RtlRestoreContext with STATUS_UNWIND_CONSOLIDATE: the callback runs
 * below the target frame and returns where to continue. With raise set,
 * the callback throws and the target frame's __except catches it, which
 * needs unwinding through the consolidation frame. */
static CTX target;
static volatile int stage;
static void *consolidate_callback(RECORD *r) {
    stage = 2;
    if (r->info[1]) seh_raise(0xE0000042, 0);
    return (void *)r->info[2];
}
__declspec(noinline) int seh_consolidate(int raise) {
    volatile int caught = 0;
    stage = 0;
    __try {
        RtlCaptureContext(&target);
        if (stage == 0) {
            RECORD rec = {0};
            stage = 1;
            rec.code = 0x80000029; /* STATUS_UNWIND_CONSOLIDATE */
            rec.count = 3;
            rec.info[0] = (U64)consolidate_callback;
            rec.info[1] = (U64)raise;
            rec.info[2] = RIP(&target);
            RtlRestoreContext(&target, &rec);
        }
    }
    __except (_exception_code() == 0xE0000042) { caught = 1; }
    return stage * 10 + caught;
}
