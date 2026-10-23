/* Frame-based SEH through __C_specific_handler, RtlUnwindEx and
 * RtlRestoreContext consolidation (cases in seh_cases.c), and fiber local
 * storage with its callbacks. The same PE runs on Windows. */
#include "common.h"

int seh_access_violation(void);
DWORD seh_software_code(void);
int seh_finally_order(void);
int seh_continue_search(void);
int seh_continue_execution(void);
int seh_finally_normal(void);
int seh_consolidate(int raise);

static volatile LONG fls_calls;
static volatile ULONG_PTR fls_last;
static void WINAPI fls_callback(PVOID value) {
    InterlockedIncrement(&fls_calls);
    fls_last = (ULONG_PTR)value;
}
static DWORD fls_index;
static DWORD WINAPI fls_thread(LPVOID p) {
    FlsSetValue(fls_index, p);
    return FlsGetValue(fls_index) == p ? 0 : 1;
}

void __stdcall start(void) {
    check_eq("hardware fault caught by __except", seh_access_violation(), 2);
    check_eq("__except sees the software exception code", seh_software_code(), 0xE0001234);
    /* __try body (1), filter (0), __finally (2) during the unwind, __except body (3). */
    check_eq("__try, filter, then __finally, then __except", seh_finally_order(), 41023);
    check_eq("declined filter passes to the outer handler", seh_continue_search(), 15);
    check_eq("EXCEPTION_CONTINUE_EXECUTION resumes", seh_continue_execution(), 7);
    check_eq("__finally on normal exit", seh_finally_normal(), 11);
    check_eq("consolidation callback chooses the continuation", seh_consolidate(0), 20);
    check_eq("exception from the consolidation callback reaches the target frame", seh_consolidate(1), 21);
    check_eq("handlers still work afterwards", seh_access_violation(), 2);

    /* Fiber local storage. */
    fls_index = FlsAlloc(fls_callback);
    check("FlsAlloc", fls_index != FLS_OUT_OF_INDEXES);
    check("new index reads NULL", FlsGetValue(fls_index) == NULL);
    check("FlsSetValue/FlsGetValue", FlsSetValue(fls_index, (PVOID)0x1111) && FlsGetValue(fls_index) == (PVOID)0x1111);
    HANDLE t = CreateThread(NULL, 0, fls_thread, (PVOID)0x2222, 0, NULL);
    DWORD code = 1;
    check("thread has its own value", t && WaitForSingleObject(t, 5000) == WAIT_OBJECT_0 && GetExitCodeThread(t, &code) && code == 0);
    CloseHandle(t);
    check("callback ran at thread exit with its value", fls_calls == 1 && fls_last == 0x2222);
    check("this thread's value is untouched", FlsGetValue(fls_index) == (PVOID)0x1111);
    check("FlsFree runs the callback for remaining values", FlsFree(fls_index) && fls_calls == 2 && fls_last == 0x1111);
    SetLastError(0);
    check("index beyond the maximum is invalid", !FlsSetValue(4090, (PVOID)1) && GetLastError() == ERROR_INVALID_PARAMETER);
    finish("seh");
}
