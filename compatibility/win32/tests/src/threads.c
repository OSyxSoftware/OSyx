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

typedef LONG (NTAPI *keyed_create_fn)(HANDLE *, ULONG, void *, ULONG);
typedef LONG (NTAPI *keyed_op_fn)(HANDLE, ULONG_PTR, BOOLEAN, const LARGE_INTEGER *);
static keyed_op_fn keyed_wait, keyed_release;
struct keyed_job { HANDLE handle; ULONG_PTR key; int release; unsigned loops; };
static DWORD WINAPI keyed_worker(LPVOID arg) {
    struct keyed_job *job = arg;
    LARGE_INTEGER timeout; timeout.QuadPart = -10000000;
    for (unsigned n = 0; n < job->loops; n++) {
        LONG status = (job->release ? keyed_release : keyed_wait)(job->handle, job->key, FALSE, &timeout);
        if (status != 0) return (DWORD)status;
    }
    return 0;
}
static int keyed_join(HANDLE thread) {
    DWORD status = 99;
    int ok = thread && WaitForSingleObject(thread, 5000) == WAIT_OBJECT_0 && GetExitCodeThread(thread, &status) && status == 0;
    if (thread) CloseHandle(thread);
    return ok;
}
static void keyed_events(void) {
    HMODULE nt = GetModuleHandleW(L"ntdll.dll");
    keyed_create_fn create = (keyed_create_fn)GetProcAddress(nt,"NtCreateKeyedEvent");
    keyed_wait = (keyed_op_fn)GetProcAddress(nt,"NtWaitForKeyedEvent");
    keyed_release = (keyed_op_fn)GetProcAddress(nt,"NtReleaseKeyedEvent");
    check("NT keyed-event exports",create && keyed_wait && keyed_release);
    if (!create || !keyed_wait || !keyed_release) return;
    HANDLE h = NULL;
    check("NtCreateKeyedEvent creates a closable object",create(&h,0x1f0003,NULL,0)==0 && h);
    if (!h) return;
    LARGE_INTEGER poll, timeout; poll.QuadPart=0; timeout.QuadPart=-10000000;
    check("keyed wait without release times out",keyed_wait(h,0x1000,FALSE,&poll)==0x102);
    check("keyed release is not a stored signal",keyed_release(h,0x1000,FALSE,&poll)==0x102);
    LARGE_INTEGER absolute; absolute.QuadPart=1;
    check("keyed events support an expired absolute deadline",keyed_wait(h,0x1000,FALSE,&absolute)==0x102);
    FILETIME now; GetSystemTimeAsFileTime(&now);
    absolute.QuadPart=(LONGLONG)(((ULONGLONG)now.dwHighDateTime<<32)|now.dwLowDateTime)+100000;
    check("keyed events support a future absolute deadline",keyed_wait(h,0x1000,FALSE,&absolute)==0x102);
    check("keyed events reject odd keys",keyed_wait(h,0x1001,FALSE,&poll)==(LONG)0xc000000d);
    struct keyed_job job = {h,0x1000,0,1};
    HANDLE worker = CreateThread(NULL,0,keyed_worker,&job,0,NULL);
    Sleep(20);
    LONG paired=keyed_release(h,job.key,FALSE,&timeout); int joined=keyed_join(worker);
    check("release wakes exactly a matching keyed waiter",paired==0 && joined);
    job.release=1;
    worker=CreateThread(NULL,0,keyed_worker,&job,0,NULL);
    Sleep(20);
    paired=keyed_wait(h,job.key,FALSE,&timeout); joined=keyed_join(worker);
    check("keyed release can arrive before its waiter",paired==0 && joined);
    timeout.QuadPart=-100000;
    check("a timed-out keyed waiter is removed",keyed_wait(h,0x2000,FALSE,&timeout)==0x102 && keyed_release(h,0x2000,FALSE,&poll)==0x102);
    HANDLE duplicate=NULL;
    check("DuplicateHandle preserves keyed-event identity",DuplicateHandle(GetCurrentProcess(),h,GetCurrentProcess(),&duplicate,0,FALSE,DUPLICATE_SAME_ACCESS));
    job.handle=duplicate; job.release=0;
    worker=CreateThread(NULL,0,keyed_worker,&job,0,NULL);
    timeout.QuadPart=-10000000;
    paired=keyed_release(h,job.key,FALSE,&timeout); joined=keyed_join(worker);
    check("keyed-event duplicates rendezvous on the same object",paired==0 && joined);
    if (duplicate) CloseHandle(duplicate);
    struct keyed_job jobs[4]; HANDLE workers[4]; int ok=1;
    for (unsigned i=0;i<4;i++) {
        jobs[i]=(struct keyed_job){h,0x3000+2*i,0,128};
        workers[i]=CreateThread(NULL,0,keyed_worker,&jobs[i],0,NULL);
        ok &= workers[i]!=NULL;
    }
    for (unsigned n=0;n<128 && ok;n++) for (unsigned i=0;i<4 && ok;i++) ok &= keyed_release(h,jobs[i].key,FALSE,&timeout)==0;
    for (unsigned i=0;i<4;i++) ok &= keyed_join(workers[i]);
    check("four keyed waiters keep 512 rendezvous separate",ok);
    check("CloseHandle releases a keyed event",CloseHandle(h));
    check("closed keyed-event handle is rejected",keyed_wait(h,0x1000,FALSE,&poll)==(LONG)0xc0000008);
}

void __stdcall start(void) {
    /* NSPR attaches a thread by duplicating GetCurrentThread(), which is a
     * pseudo-handle rather than a handle: Windows allows that. */
    HANDLE self = NULL;
    check("GetCurrentThread pseudo-handle duplicates",
          DuplicateHandle(GetCurrentProcess(), GetCurrentThread(), GetCurrentProcess(), &self, 0, FALSE, DUPLICATE_SAME_ACCESS) &&
              self != NULL && GetThreadId(self) == GetCurrentThreadId());
    if (self) CloseHandle(self);
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
        /* OSyx emulates private RWX pages using W^X permission switches. */
        BOOL rwx = VirtualProtect(p, 4096, PAGE_EXECUTE_READWRITE, &old);
        check("PAGE_EXECUTE_READWRITE private memory", rwx);
        if (rwx) {
            BYTE *code = (BYTE *)p;
            code[0] = 0xB8; code[1] = 42; code[2] = code[3] = code[4] = 0; code[5] = 0xC3;
            FlushInstructionCache(GetCurrentProcess(), code, 6);
            int (*run)(void) = (int (*)(void))code;
            check("execute generated code", run() == 42);
            code[1] = 43;
            FlushInstructionCache(GetCurrentProcess(), code, 6);
            check("modify and reexecute generated code", run() == 43);
            check("VirtualQuery retains logical RWX", VirtualQuery(p, &mbi, sizeof mbi) == sizeof mbi && mbi.Protect == PAGE_EXECUTE_READWRITE);
            check("switch JIT memory to RX", VirtualProtect(p, 4096, PAGE_EXECUTE_READ, &old) && old == PAGE_EXECUTE_READWRITE);
            check("execute after explicit RX", run() == 43);
        }
        check("VirtualFree(MEM_RELEASE)", VirtualFree(p, 0, MEM_RELEASE));
    }

    BYTE *jit = VirtualAlloc(NULL, 128 * 1024, MEM_RESERVE, PAGE_NOACCESS);
    BYTE *committed = jit ? VirtualAlloc(jit + 4096, 65536, MEM_COMMIT, PAGE_EXECUTE_READWRITE) : NULL;
    check("commit RWX inside a reservation", committed != NULL && committed == jit + 4096);
    if (committed) {
        committed[0] = 0xB8; committed[1] = 7; committed[2] = committed[3] = committed[4] = 0; committed[5] = 0xC3;
        FlushInstructionCache(GetCurrentProcess(), committed, 6);
        check("execute committed JIT page", ((int (*)(void))committed)() == 7);
        check("decommit JIT page", VirtualFree(committed, 4096, MEM_DECOMMIT));
        check("recommit JIT page as writable data", VirtualAlloc(committed, 4096, MEM_COMMIT, PAGE_READWRITE) == committed && committed[0] == 0);
    }
    if (jit) check("release JIT reservation", VirtualFree(jit, 0, MEM_RELEASE));

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
    keyed_events();
    DeleteCriticalSection(&lock);
    finish("threads");
}
