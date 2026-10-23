/* Target D: threads and synchronisation: CreateThread, WaitForSingleObject
 * / WaitForMultipleObjects, exit codes, critical sections, events,
 * Sleep, TLS slots, implicit (__thread) TLS, interlocked
 * operations and virtual memory. Also loads windll.dll, which imports
 * through an API set contract and forwards an export to kernel32. */
#include "common.h"

/* Implicit TLS without a C runtime: what the CRT would provide. */
ULONG _tls_index = 0;
__attribute__((section(".tls"))) char _tls_start = 0;
__attribute__((section(".tls$ZZZ"))) char _tls_end = 0;
static void NTAPI on_tls(PVOID module, DWORD reason, PVOID reserved);
__attribute__((section(".CRT$XLB"), used)) PIMAGE_TLS_CALLBACK tls_callback_entry = on_tls;
__attribute__((section(".CRT$XLZ"), used)) PIMAGE_TLS_CALLBACK tls_callback_end = 0;
__attribute__((used)) const IMAGE_TLS_DIRECTORY64 _tls_used = {
    (ULONGLONG)&_tls_start, (ULONGLONG)&_tls_end, (ULONGLONG)&_tls_index, (ULONGLONG)&tls_callback_entry, 0, 0,
};

static volatile LONG tls_attach_calls, thread_attach_calls;
static void NTAPI on_tls(PVOID module, DWORD reason, PVOID reserved) {
    (void)module;
    (void)reserved;
    if (reason == DLL_PROCESS_ATTACH) InterlockedIncrement(&tls_attach_calls);
    if (reason == DLL_THREAD_ATTACH) InterlockedIncrement(&thread_attach_calls);
}

/* Native PE TLS (.tls section, _tls_index), not emulated TLS. */
__thread int per_thread = 7;

#define THREADS 4
#define ROUNDS 20000
static CRITICAL_SECTION lock;
static volatile LONG plain_counter; /* updated under `lock` */
static volatile LONG atomic_counter;
static DWORD tls_slot;
static HANDLE go;

static DWORD WINAPI worker(LPVOID arg) {
    DWORD id = (DWORD)(ULONG_PTR)arg;
    if (per_thread != 7) return 1000; /* fresh copy of the TLS template */
    per_thread = 100 + id;
    TlsSetValue(tls_slot, (LPVOID)(ULONG_PTR)(id + 1));
    WaitForSingleObject(go, INFINITE);
    for (int i = 0; i < ROUNDS; i++) {
        EnterCriticalSection(&lock);
        plain_counter = plain_counter + 1;
        LeaveCriticalSection(&lock);
        InterlockedIncrement(&atomic_counter);
        if (i % 5000 == 0) Sleep(1);
    }
    if (per_thread != (int)(100 + id) || TlsGetValue(tls_slot) != (LPVOID)(ULONG_PTR)(id + 1)) return 2000;
    return 10 + id;
}

typedef int(__cdecl *mul_fn)(int, int);
typedef DWORD(WINAPI *tick_fn)(void);
typedef DWORD(__cdecl *attach_fn)(void);

void __stdcall start(void) {
    check_eq("TLS callback ran for process attach", tls_attach_calls, 1);
    check_eq("implicit TLS initial value", per_thread, 7);
    InitializeCriticalSection(&lock);
    tls_slot = TlsAlloc();
    check("TlsAlloc", tls_slot != TLS_OUT_OF_INDEXES);
    go = CreateEventA(NULL, TRUE, FALSE, NULL);
    check("CreateEventA (manual reset)", go != NULL);

    HANDLE t[THREADS];
    DWORD ids[THREADS];
    for (DWORD i = 0; i < THREADS; i++) t[i] = CreateThread(NULL, 0, worker, (LPVOID)(ULONG_PTR)i, 0, &ids[i]);
    check("CreateThread x4", t[0] && t[1] && t[2] && t[3] && ids[0] != ids[1]);
    check_eq("a busy thread is not signalled", WaitForSingleObject(t[0], 50), WAIT_TIMEOUT);
    DWORD t0 = GetTickCount();
    SetEvent(go);
    check_eq("WaitForMultipleObjects(all)", WaitForMultipleObjects(THREADS, t, TRUE, 30000), WAIT_OBJECT_0);
    DWORD took = GetTickCount() - t0;
    int codes_ok = 1;
    for (DWORD i = 0; i < THREADS; i++) {
        DWORD code = 0;
        codes_ok &= GetExitCodeThread(t[i], &code) && code == 10 + i;
        CloseHandle(t[i]);
    }
    check("thread exit codes (TLS slots and implicit TLS per thread)", codes_ok);
    check_eq("critical section kept the counter exact", plain_counter, THREADS * ROUNDS);
    check_eq("InterlockedIncrement", atomic_counter, THREADS * ROUNDS);
    check_eq("main thread kept its implicit TLS", per_thread, 7);
    check_eq("TLS callback ran for each thread attach", thread_attach_calls, THREADS);
    put("4 threads x 20000 locked increments: ");
    put_uint(took);
    put(" ms\r\n");

    HANDLE auto_ev = CreateEventA(NULL, FALSE, TRUE, NULL);
    check_eq("auto-reset event: first wait", WaitForSingleObject(auto_ev, 0), WAIT_OBJECT_0);
    check_eq("auto-reset event: reset after one waiter", WaitForSingleObject(auto_ev, 0), WAIT_TIMEOUT);
    CloseHandle(auto_ev);
    DWORD s0 = GetTickCount();
    Sleep(100);
    DWORD slept = GetTickCount() - s0;
    check("Sleep(100) sleeps 100..300 ms", slept >= 100 && slept < 300);

    /* Virtual memory: reserve+commit, write, protect read-only, free. */
    char *p = VirtualAlloc(NULL, 65536, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
    check("VirtualAlloc(64 KiB, PAGE_READWRITE)", p != NULL && ((ULONG_PTR)p & 0xFFFF) == 0);
    if (p) {
        p[0] = 1;
        p[65535] = 2;
        DWORD old = 0;
        check("VirtualProtect(PAGE_READONLY)", VirtualProtect(p, 4096, PAGE_READONLY, &old) && old == PAGE_READWRITE);
        MEMORY_BASIC_INFORMATION mbi;
        check("VirtualQuery reports the protection", VirtualQuery(p, &mbi, sizeof mbi) == sizeof mbi && mbi.Protect == PAGE_READONLY && mbi.State == MEM_COMMIT);
        /* A deliberate difference: Windows grants RWX pages, OSyx enforces W^X. */
        char e[4];
        int on_osyx = GetEnvironmentVariableA("OSYX", e, sizeof e) == 1;
        BOOL rwx = VirtualProtect(p, 4096, PAGE_EXECUTE_READWRITE, &old);
        check(on_osyx ? "PAGE_EXECUTE_READWRITE refused (OSyx W^X policy)" : "PAGE_EXECUTE_READWRITE granted (Windows)", on_osyx ? !rwx : rwx);
        check("VirtualFree(MEM_RELEASE)", VirtualFree(p, 0, MEM_RELEASE));
    }

    /* A DLL from the file system, with its own imports and a forwarder. */
    HMODULE dll = LoadLibraryA("windll.dll");
    check("LoadLibraryA(\"windll.dll\") from the program's directory", dll != NULL);
    if (dll) {
        mul_fn mul = (mul_fn)GetProcAddress(dll, "multiply");
        check_eq("windll!multiply(6, 7)", mul ? (unsigned long long)mul(6, 7) : 0, 42);
        check("forwarded export windll!Ticks is kernel32!GetTickCount", GetProcAddress(dll, "Ticks") == GetProcAddress(GetModuleHandleA("kernel32.dll"), "GetTickCount"));
        attach_fn attaches = (attach_fn)GetProcAddress(dll, "attach_count");
        check_eq("DllMain saw one process attach", attaches ? attaches() : 0, 1);
        check("by-ordinal GetProcAddress", GetProcAddress(dll, (LPCSTR)(ULONG_PTR)5) == (FARPROC)attaches);
        check("FreeLibrary", FreeLibrary(dll));
    }
    SetLastError(0);
    check("LoadLibraryA of a missing DLL fails", LoadLibraryA("nonexistent.dll") == NULL);
    check_eq("... with ERROR_MOD_NOT_FOUND", GetLastError(), ERROR_MOD_NOT_FOUND);
    DeleteCriticalSection(&lock);
    finish("threads");
}
