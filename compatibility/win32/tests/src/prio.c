/* Critical sections and priority inheritance. A LOWEST thread holds a
 * critical section while a NORMAL thread keeps the same CPU busy and a
 * HIGHEST thread waits for the section; afterwards several threads of both
 * priorities contend for one section on every CPU. On OSyx the kernel32
 * extension OsyxThreadPriorityInfo shows the scheduler's view (own,
 * effective, boost and inherited priority, one byte each), so inheritance
 * and its end are checked exactly; on Windows, which has no such export,
 * only the behaviour is checked. */
#include "common.h"

typedef ULONGLONG (WINAPI *info_fn)(DWORD);

/* osyx_abi::priority levels of THREAD_PRIORITY_LOWEST/NORMAL/HIGHEST. */
#define LEVEL_LOW 1
#define LEVEL_NORMAL 3
#define LEVEL_HIGH 5

static info_fn info_of;
static CRITICAL_SECTION cs;
static int cpu;
static volatile LONG held, release_flag, stop_flag;
static volatile LONGLONG progress;
static DWORD low_id;
static ULONGLONG low_after, high_after;
static DWORD high_waited;
static LONGLONG released_at, acquired_at;

static unsigned base_of(ULONGLONG v) { return (unsigned)(v & 0xff); }
static unsigned effective_of(ULONGLONG v) { return (unsigned)(v >> 8 & 0xff); }
static unsigned inherited_of(ULONGLONG v) { return (unsigned)(v >> 24 & 0xff); }

static LONGLONG now_us(void) {
    LARGE_INTEGER c, f;
    QueryPerformanceCounter(&c);
    QueryPerformanceFrequency(&f);
    return c.QuadPart * 1000000 / f.QuadPart;
}

static void pin(int to) { SetThreadAffinityMask(GetCurrentThread(), (DWORD_PTR)1 << to); }

static DWORD WINAPI low_thread(void *arg) {
    (void)arg;
    SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_LOWEST);
    pin(cpu);
    low_id = GetCurrentThreadId();
    EnterCriticalSection(&cs);
    InterlockedExchange(&held, 1);
    while (!release_flag) progress++;
    LeaveCriticalSection(&cs);
    if (info_of) low_after = info_of(low_id);
    return 0;
}

static DWORD WINAPI medium_thread(void *arg) {
    (void)arg;
    pin(cpu);
    while (!stop_flag) YieldProcessor();
    return 0;
}

static DWORD WINAPI high_thread(void *arg) {
    (void)arg;
    SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
    pin(cpu);
    DWORD start = GetTickCount();
    EnterCriticalSection(&cs);
    acquired_at = now_us();
    high_waited = GetTickCount() - start;
    LeaveCriticalSection(&cs);
    if (info_of) high_after = info_of(GetCurrentThreadId());
    return 0;
}

static int wait_inherited(DWORD tid, unsigned want, DWORD limit_ms) {
    DWORD start = GetTickCount();
    while (GetTickCount() - start < limit_ms) {
        if (inherited_of(info_of(tid)) == want) return 1;
        Sleep(1);
    }
    return 0;
}

static void inversion(void) {
    InitializeCriticalSection(&cs);
    HANDLE low = CreateThread(NULL, 0, low_thread, NULL, 0, NULL);
    DWORD start = GetTickCount();
    while (!held && GetTickCount() - start < 5000) Sleep(1);
    check("LOWEST thread holds the critical section", held != 0);
    HANDLE medium = CreateThread(NULL, 0, medium_thread, NULL, 0, NULL);
    Sleep(50);
    LONGLONG p0 = progress;
    Sleep(200);
    LONGLONG starved = progress - p0;
    ULONGLONG before = info_of ? info_of(low_id) : 0;
    HANDLE high = CreateThread(NULL, 0, high_thread, NULL, 0, NULL);
    int lent = 1;
    ULONGLONG during = 0;
    if (info_of) {
        lent = wait_inherited(low_id, LEVEL_HIGH, 2000);
        during = info_of(low_id);
    } else {
        Sleep(50);
    }
    LONGLONG p1 = progress;
    Sleep(200);
    LONGLONG inheriting = progress - p1;
    released_at = now_us();
    InterlockedExchange(&release_flag, 1);
    int high_done = WaitForSingleObject(high, 5000) == WAIT_OBJECT_0;
    int low_done = WaitForSingleObject(low, 5000) == WAIT_OBJECT_0;
    InterlockedExchange(&stop_flag, 1);
    WaitForSingleObject(medium, 5000);
    CloseHandle(high);
    CloseHandle(low);
    CloseHandle(medium);
    put("prio: LOWEST progress per 200 ms: ");
    put_uint((unsigned long long)starved);
    put(" before HIGHEST waits, ");
    put_uint((unsigned long long)inheriting);
    put(" while it waits; unlock to HIGHEST ");
    put_uint((unsigned long long)(acquired_at - released_at));
    put(" us; HIGHEST waited ");
    put_uint(high_waited);
    put(" ms\r\n");
    check("HIGHEST thread gets the critical section after the release", high_done && low_done);
    if (info_of) {
        check_eq("LOWEST holder has nothing inherited before HIGHEST waits", inherited_of(before), 0);
        check("LOWEST holder inherits HIGHEST while HIGHEST waits in EnterCriticalSection", lent);
        check_eq("holder's effective priority is HIGHEST while it inherits", effective_of(during), LEVEL_HIGH);
        check_eq("holder keeps its own LOWEST priority while it inherits", base_of(during), LEVEL_LOW);
        check("inheriting holder outruns the NORMAL thread on its CPU", inheriting > starved * 4);
        check_eq("LeaveCriticalSection ends the inheritance", inherited_of(low_after), 0);
        check("holder's effective priority is back to LOWEST after leaving", effective_of(low_after) <= LEVEL_LOW + 1);
        check_eq("HIGHEST waiter keeps its own priority", base_of(high_after), LEVEL_HIGH);
        check_eq("HIGHEST waiter inherited nothing", inherited_of(high_after), 0);
    }
    DeleteCriticalSection(&cs);
}

#define WORKERS_MAX 32
#define ROUNDS 2000
static CRITICAL_SECTION shared;
static volatile LONG counter_plain;
static volatile LONG finished, release_all;
static ULONGLONG final_view[WORKERS_MAX];
static int worker_cpu[WORKERS_MAX], worker_level[WORKERS_MAX];

static DWORD WINAPI load_worker(void *arg) {
    int n = (int)(INT_PTR)arg;
    SetThreadPriority(GetCurrentThread(), worker_level[n]);
    pin(worker_cpu[n]);
    for (int i = 0; i < ROUNDS; i++) {
        EnterCriticalSection(&shared);
        /* Recursion must keep working with inheritance in place. */
        if (i % 50 == 0) EnterCriticalSection(&shared);
        LONG v = counter_plain;
        for (int k = 0; k < (worker_level[n] == THREAD_PRIORITY_LOWEST ? 400 : 40); k++) YieldProcessor();
        counter_plain = v + 1;
        if (i % 50 == 0) LeaveCriticalSection(&shared);
        LeaveCriticalSection(&shared);
    }
    InterlockedIncrement(&finished);
    while (!release_all) Sleep(2);
    if (info_of) final_view[n] = info_of(GetCurrentThreadId());
    return 0;
}

static DWORD WINAPI spinner(void *arg) {
    pin((int)(INT_PTR)arg);
    while (!stop_flag) YieldProcessor();
    return 0;
}

static void load(int cpus) {
    InitializeCriticalSection(&shared);
    stop_flag = 0;
    int workers = 4 * cpus;
    if (workers > WORKERS_MAX) workers = WORKERS_MAX;
    HANDLE spin[16], h[WORKERS_MAX];
    for (int c = 0; c < cpus && c < 16; c++) spin[c] = CreateThread(NULL, 0, spinner, (void *)(INT_PTR)c, 0, NULL);
    for (int n = 0; n < workers; n++) {
        worker_cpu[n] = n % cpus;
        worker_level[n] = n / cpus % 2 ? THREAD_PRIORITY_HIGHEST : THREAD_PRIORITY_LOWEST;
        h[n] = CreateThread(NULL, 0, load_worker, (void *)(INT_PTR)n, 0, NULL);
    }
    DWORD start = GetTickCount();
    while (finished < workers && GetTickCount() - start < 60000) Sleep(5);
    DWORD took = GetTickCount() - start;
    int all_finished = finished == workers;
    InterlockedExchange(&release_all, 1);
    int joined = WaitForMultipleObjects((DWORD)workers, h, TRUE, 10000) == WAIT_OBJECT_0;
    InterlockedExchange(&stop_flag, 1);
    WaitForMultipleObjects((DWORD)(cpus < 16 ? cpus : 16), spin, TRUE, 10000);
    put("prio: ");
    put_uint((unsigned long long)workers);
    put(" threads x ");
    put_uint(ROUNDS);
    put(" critical sections on ");
    put_uint((unsigned long long)cpus);
    put(" CPUs in ");
    put_uint(took);
    put(" ms\r\n");
    check("every contending thread finishes (no deadlock)", all_finished && joined);
    check_eq("no update is lost under contention", (unsigned long long)counter_plain, (unsigned long long)workers * ROUNDS);
    if (info_of) {
        int clean = 1;
        for (int n = 0; n < workers; n++) clean &= inherited_of(final_view[n]) == 0;
        check("nothing stays inherited once every thread is done", clean);
    }
    for (int n = 0; n < workers; n++) CloseHandle(h[n]);
    for (int c = 0; c < cpus && c < 16; c++) CloseHandle(spin[c]);
    DeleteCriticalSection(&shared);
}

void start(void) {
    SYSTEM_INFO si;
    GetSystemInfo(&si);
    int cpus = (int)si.dwNumberOfProcessors;
    if (cpus > 16) cpus = 16;
    cpu = cpus > 1 ? cpus - 1 : 0;
    info_of = (info_fn)(void *)GetProcAddress(GetModuleHandleA("kernel32.dll"), "OsyxThreadPriorityInfo");
    pin(0);
    if (cpus < 2) {
        put("prio: one CPU, inheritance checks need two\r\n");
    } else {
        inversion();
    }
    load(cpus);
    finish("prio");
}
