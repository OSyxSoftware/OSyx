/* Target: fibers, built twice from this file: fibers.exe (PE32+) and
 * fibers32.exe (PE32, through the WoW64 bridge). ConvertThreadToFiber,
 * CreateFiber, SwitchToFiber between three fibers, GetCurrentFiber and
 * GetFiberData (the TEB's FiberData, as the Windows SDK macros read it),
 * the TEB stack bounds of each fiber, integer and floating-point state
 * across switches, imports called on a fiber's own stack, a fiber whose
 * routine returns (its thread ends), DeleteFiber and ConvertFiberToThread.
 * No C runtime.
 *
 * Prints "fibers: ok NAME" / "fibers: FAIL NAME" (fibers32: for the PE32
 * build) and a summary, and exits with the number of failures. Runs only
 * under /bin/winrun inside QEMU (scripts/win-test.ps1). */
typedef unsigned long DWORD;
typedef int BOOL;
typedef void *HANDLE;

#define STD_OUTPUT_HANDLE ((DWORD)-11)
#define WAIT_OBJECT_0 0
#define ERROR_ALREADY_FIBER 1280

__declspec(dllimport) HANDLE __stdcall GetStdHandle(DWORD std_handle);
__declspec(dllimport) BOOL __stdcall WriteFile(HANDLE file, const void *buf, DWORD len, DWORD *written, void *overlapped);
__declspec(dllimport) void __stdcall ExitProcess(DWORD code);
__declspec(dllimport) HANDLE __stdcall CreateThread(void *sa, unsigned long stack, DWORD(__stdcall *start)(void *),
                                                    void *param, DWORD flags, DWORD *tid);
__declspec(dllimport) DWORD __stdcall WaitForSingleObject(HANDLE h, DWORD ms);
__declspec(dllimport) BOOL __stdcall CloseHandle(HANDLE h);
__declspec(dllimport) DWORD __stdcall GetTickCount(void);
__declspec(dllimport) int __stdcall MulDiv(int a, int b, int c);
__declspec(dllimport) void __stdcall Sleep(DWORD ms);
__declspec(dllimport) DWORD __stdcall GetLastError(void);
__declspec(dllimport) void *__stdcall ConvertThreadToFiber(void *param);
__declspec(dllimport) BOOL __stdcall ConvertFiberToThread(void);
__declspec(dllimport) void *__stdcall CreateFiber(unsigned long stack, void(__stdcall *start)(void *), void *param);
__declspec(dllimport) void __stdcall SwitchToFiber(void *fiber);
__declspec(dllimport) void __stdcall DeleteFiber(void *fiber);
__declspec(dllimport) BOOL __stdcall IsThreadAFiber(void);

#ifdef _WIN64
#define TAG "fibers: "
/* NT_TIB of the 64-bit TEB: StackBase 0x08, StackLimit 0x10, FiberData 0x20. */
static void *current_fiber(void) {
    void *p;
    __asm__ volatile("movq %%gs:0x20, %0" : "=r"(p));
    return p;
}
static void stack_bounds(char **base, char **limit) {
    __asm__ volatile("movq %%gs:0x08, %0" : "=r"(*base));
    __asm__ volatile("movq %%gs:0x10, %0" : "=r"(*limit));
}
#else
#define TAG "fibers32: "
/* NT_TIB of the 32-bit TEB: StackBase 0x04, StackLimit 0x08, FiberData 0x10. */
static void *current_fiber(void) {
    void *p;
    __asm__ volatile("movl %%fs:0x10, %0" : "=r"(p));
    return p;
}
static void stack_bounds(char **base, char **limit) {
    __asm__ volatile("movl %%fs:0x04, %0" : "=r"(*base));
    __asm__ volatile("movl %%fs:0x08, %0" : "=r"(*limit));
}
#endif

static HANDLE out;
static int failed;

static int len(const char *s) {
    int n = 0;
    while (s[n]) n++;
    return n;
}

static void check(const char *name, int ok) {
    DWORD w;
    const char *p = ok ? TAG "ok " : TAG "FAIL ";
    WriteFile(out, p, len(p), &w, 0);
    WriteFile(out, name, len(name), &w, 0);
    WriteFile(out, "\n", 1, &w, 0);
    if (!ok) failed++;
}

static int on_own_stack(void) {
    char *base, *limit, here;
    stack_bounds(&base, &limit);
    return limit < &here && &here < base;
}

/* The fibers and what they report back to the main fiber. */
static void *main_fiber, *gen_fiber, *relay_fiber;
static volatile int yielded, gen_done, relay_hops;
static volatile int gen_identity, gen_stack, relay_identity, relay_stack, relay_imports;
static volatile double gen_float;
static char tag_main, tag_gen, tag_relay;

/* A generator: yields 1..20 to the main fiber, keeping integer and
 * floating-point state in its own frame across the switches. */
static void __stdcall generator(void *param) {
    gen_identity = param == &tag_gen && current_fiber() == gen_fiber && *(void **)current_fiber() == &tag_gen;
    gen_stack = on_own_stack();
    double acc = 0.25;
    for (int i = 1; i <= 20; i++) {
        acc = acc * 1.5 + i;
        yielded = i;
        SwitchToFiber(main_fiber);
    }
    gen_float = acc;
    gen_done = 1;
    SwitchToFiber(main_fiber);
}

/* Calls imports on its own stack between switches: main -> relay -> main,
 * five times. */
static void __stdcall relay(void *param) {
    relay_identity = param == &tag_relay && current_fiber() == relay_fiber;
    relay_stack = on_own_stack();
    int sum = 0;
    for (int i = 0; i < 5; i++) {
        sum += MulDiv(i, 3, 1);
        GetTickCount();
        Sleep(0);
        relay_hops++;
        SwitchToFiber(main_fiber);
    }
    relay_imports = sum == 30;
    SwitchToFiber(main_fiber);
}

static double expected_float(void) {
    double acc = 0.25;
    for (int i = 1; i <= 20; i++) acc = acc * 1.5 + i;
    return acc;
}

/* A thread whose fiber routine returns: the thread ends (ExitThread). */
static volatile int last_ran;
static void __stdcall last_fiber(void *param) {
    last_ran = param == (void *)0x55;
}
static DWORD __stdcall fiber_thread(void *param) {
    (void)param;
    void *self = ConvertThreadToFiber(0);
    void *f = self ? CreateFiber(0, last_fiber, (void *)0x55) : 0;
    if (f) SwitchToFiber(f);
    return 0x77; /* not reached when the fiber routine ends the thread */
}

void __stdcall start(void) {
    out = GetStdHandle(STD_OUTPUT_HANDLE);
    check("a thread is not a fiber before ConvertThreadToFiber", !IsThreadAFiber());
    main_fiber = ConvertThreadToFiber(&tag_main);
    check("ConvertThreadToFiber returns the fiber",
          main_fiber && current_fiber() == main_fiber && *(void **)main_fiber == &tag_main && IsThreadAFiber());
    check("ConvertThreadToFiber twice fails with ERROR_ALREADY_FIBER",
          !ConvertThreadToFiber(0) && GetLastError() == ERROR_ALREADY_FIBER);

    gen_fiber = CreateFiber(0, generator, &tag_gen);
    relay_fiber = CreateFiber(64 * 1024, relay, &tag_relay);
    check("CreateFiber returns fibers with their parameters",
          gen_fiber && relay_fiber && *(void **)gen_fiber == &tag_gen && *(void **)relay_fiber == &tag_relay);

    /* Interleave both fibers; main keeps its own state in locals that live
     * in callee-saved registers across the calls. */
    int sum = 0, order = 0, ok_order = 1;
    unsigned mix = 0x12345678;
    for (int round = 0; round < 25 && gen_fiber && relay_fiber; round++) {
        if (!gen_done) {
            SwitchToFiber(gen_fiber);
            if (!gen_done) {
                sum += yielded;
                if (yielded != ++order) ok_order = 0;
            }
        }
        if (relay_hops < 5) SwitchToFiber(relay_fiber);
        mix = mix * 1103515245u + 12345u;
    }
    unsigned expect = 0x12345678;
    for (int round = 0; round < 25; round++) expect = expect * 1103515245u + 12345u;
    check("main fiber resumes after each switch", current_fiber() == main_fiber && *(void **)current_fiber() == &tag_main);
    check("generator yields 1..20 in order", ok_order && sum == 210 && order == 20);
    check("generator ran to its end", gen_done);
    check("GetCurrentFiber and GetFiberData inside a fiber", gen_identity && relay_identity);
    check("each fiber runs on its own stack (TEB stack bounds)", gen_stack && relay_stack);
    check("main fiber's stack bounds restored", on_own_stack());
    check("floating point state kept across switches", gen_float == expected_float());
    check("main fiber's registers kept across switches", mix == expect);
    check("imports called on a fiber's stack", relay_hops == 5);
    SwitchToFiber(relay_fiber);
    check("relay fiber computed through imports", relay_imports);

    DeleteFiber(gen_fiber);
    DeleteFiber(relay_fiber);
    check("main fiber unchanged after DeleteFiber", current_fiber() == main_fiber);

    DWORD tid = 0;
    HANDLE t = CreateThread(0, 0, fiber_thread, 0, 0, &tid);
    check("a fiber routine that returns ends its thread",
          t && WaitForSingleObject(t, 10000) == WAIT_OBJECT_0 && last_ran);
    if (t) CloseHandle(t);

    check("ConvertFiberToThread", ConvertFiberToThread() && !IsThreadAFiber());
    DWORD w;
    WriteFile(out, failed ? TAG "summary FAIL\n" : TAG "summary ok\n", len(failed ? TAG "summary FAIL\n" : TAG "summary ok\n"), &w, 0);
    ExitProcess(failed);
}
