/* NT runtime pieces mozglue/vcruntime need at start-up: Rtl heap and
 * counted strings, SLists, pointer encoding, table-based unwinding and
 * back traces, calendar/interrupt/process times, version checks, pointer
 * probes, SearchPath, FormatMessage and the registry. Same PE on Windows. */
#include "common.h"
#include <winternl.h>

PVOID NTAPI RtlReAllocateHeap(PVOID, ULONG, PVOID, SIZE_T);
NTSTATUS NTAPI RtlDuplicateUnicodeString(ULONG, const UNICODE_STRING *, UNICODE_STRING *);
LONG NTAPI RtlCompareUnicodeString(const UNICODE_STRING *, const UNICODE_STRING *, BOOLEAN);
BOOLEAN NTAPI RtlEqualUnicodeString(const UNICODE_STRING *, const UNICODE_STRING *, BOOLEAN);
NTSTATUS NTAPI RtlRunOnceExecuteOnce(PRTL_RUN_ONCE, PRTL_RUN_ONCE_INIT_FN, PVOID, PVOID *);

static ULONG CALLBACK once_fn(PRTL_RUN_ONCE o, PVOID p, PVOID *ctx) {
    (void)o; (*(int *)p)++; if (ctx) *ctx = (PVOID)0x1230; return TRUE;
}

/* Frames for the back trace: not inlined, each with its own unwind data. */
static __attribute__((noinline)) USHORT level3(PVOID *frames) {
    volatile int keep = 0; /* no tail call: level3's own frame is frame 0 */
    USHORT n = RtlCaptureStackBackTrace(0, 16, frames, NULL);
    return n + (USHORT)keep;
}
static __attribute__((noinline)) USHORT level2(PVOID *frames) { volatile int pad[8]; pad[0] = 1; USHORT n = level3(frames); return n + (USHORT)(pad[0] - 1); }
static __attribute__((noinline)) USHORT level1(PVOID *frames) { volatile int pad[20]; pad[3] = 2; USHORT n = level2(frames); return n + (USHORT)(pad[3] - 2); }

typedef struct { SLIST_ENTRY entry; int value; } __attribute__((aligned(16))) Item;

static int wstr_eq(const WCHAR *a, const WCHAR *b) { while (*a && *a == *b) { a++; b++; } return *a == *b; }

void __stdcall start(void) {
    /* Rtl heap on the process heap. */
    HANDLE heap = GetProcessHeap();
    BYTE *p = RtlAllocateHeap(heap, HEAP_ZERO_MEMORY, 100);
    check("RtlAllocateHeap zeroed", p && p[0] == 0 && p[99] == 0);
    if (p) { p[5] = 7; p = RtlReAllocateHeap(heap, 0, p, 5000); }
    check("RtlReAllocateHeap keeps contents", p && p[5] == 7);
    check("RtlFreeHeap", p && RtlFreeHeap(heap, 0, p));

    /* Counted strings. */
    UNICODE_STRING a, b, dup;
    RtlInitUnicodeString(&a, L"Mozilla");
    check("RtlInitUnicodeString lengths", a.Length == 14 && a.MaximumLength == 16);
    RtlInitUnicodeString(&b, L"MOZILLA");
    check("case-insensitive compare equal", RtlCompareUnicodeString(&a, &b, TRUE) == 0 && RtlEqualUnicodeString(&a, &b, TRUE));
    check("case-sensitive compare orders", RtlCompareUnicodeString(&a, &b, FALSE) > 0 && !RtlEqualUnicodeString(&a, &b, FALSE));
    check("RtlDuplicateUnicodeString", RtlDuplicateUnicodeString(1, &a, &dup) == 0 && dup.Length == 14 && dup.Buffer[7] == 0 && wstr_eq(dup.Buffer, L"Mozilla"));
    RtlFreeUnicodeString(&dup);
    check("RtlFreeUnicodeString clears", dup.Buffer == NULL && dup.Length == 0);
    ANSI_STRING as; UNICODE_STRING conv;
    RtlInitAnsiString(&as, "gecko");
    check("RtlAnsiStringToUnicodeString", RtlAnsiStringToUnicodeString(&conv, &as, TRUE) == 0 && conv.Length == 10 && wstr_eq(conv.Buffer, L"gecko"));
    RtlFreeUnicodeString(&conv);
    check("RtlCompareMemory counts the matching prefix", RtlCompareMemory("abcdef", "abcxef", 6) == 3);

    RTL_RUN_ONCE once = RTL_RUN_ONCE_INIT; int runs = 0; PVOID ctx = NULL;
    check("RtlRunOnceExecuteOnce runs once", RtlRunOnceExecuteOnce(&once, once_fn, &runs, &ctx) == 0 &&
          RtlRunOnceExecuteOnce(&once, once_fn, &runs, &ctx) == 0 && runs == 1 && ctx == (PVOID)0x1230);

    /* Interlocked singly linked list. */
    static SLIST_HEADER head __attribute__((aligned(16)));
    static Item items[3];
    InitializeSListHead(&head);
    for (int i = 0; i < 3; i++) { items[i].value = i + 10; InterlockedPushEntrySList(&head, &items[i].entry); }
    check("SList depth", QueryDepthSList(&head) == 3);
    Item *top = (Item *)InterlockedPopEntrySList(&head);
    check("SList pops last pushed", top && top->value == 12 && QueryDepthSList(&head) == 2);
    Item *rest = (Item *)InterlockedFlushSList(&head);
    check("SList flush returns the chain", rest && rest->value == 11 && ((Item *)rest->entry.Next)->value == 10 && QueryDepthSList(&head) == 0);
    check("SList empty pop", InterlockedPopEntrySList(&head) == NULL);

    PVOID secret = (PVOID)0x12345678abcdULL;
    PVOID enc = EncodePointer(secret);
    check("EncodePointer changes and DecodePointer restores", enc != secret && DecodePointer(enc) == secret);
    check("FlushInstructionCache", FlushInstructionCache(GetCurrentProcess(), (PVOID)start, 16));

    /* Unwinding: our own function has unwind data and unwinds to a caller. */
    DWORD64 base = 0;
    PRUNTIME_FUNCTION fn = RtlLookupFunctionEntry((DWORD64)level1, &base, NULL);
    check("RtlLookupFunctionEntry finds .pdata", fn && base == (DWORD64)GetModuleHandleW(NULL) && base + fn->BeginAddress <= (DWORD64)level1);
    PVOID frames[16] = {0};
    USHORT n = level1(frames);
    MEMORY_BASIC_INFORMATION mbi;
    check("back trace has the nested frames", n >= 3 && frames[0] && frames[1] && frames[2]);
    int inside = n >= 3;
    for (int i = 0; i < 3 && inside; i++) {
        PRUNTIME_FUNCTION f = RtlLookupFunctionEntry((DWORD64)frames[i], &base, NULL);
        inside = f != NULL && VirtualQuery(frames[i], &mbi, sizeof mbi) && mbi.AllocationBase == GetModuleHandleW(NULL);
    }
    check("back trace frames are in this image", inside);
    PRUNTIME_FUNCTION f2 = RtlLookupFunctionEntry((DWORD64)frames[1], &base, NULL);
    PRUNTIME_FUNCTION f3 = RtlLookupFunctionEntry((DWORD64)level2, &base, NULL);
    check("frame 1 is inside level2", f2 && f2 == f3);
    static CONTEXT cx __attribute__((aligned(16)));
    RtlCaptureContext(&cx);
    check("RtlCaptureContext", cx.Rip != 0 && cx.Rsp != 0 && (cx.ContextFlags & 0x100003) == 0x100003);
    static CONTEXT tc __attribute__((aligned(16)));
    tc.ContextFlags = CONTEXT_CONTROL;
    check("GetThreadContext of the current thread", GetThreadContext(GetCurrentThread(), &tc) && tc.Rsp != 0);

    /* Times. */
    SYSTEMTIME st; FILETIME ft, now;
    GetSystemTime(&st);
    GetSystemTimeAsFileTime(&now);
    check("GetSystemTime plausible", st.wYear >= 2024 && st.wMonth >= 1 && st.wMonth <= 12 && st.wDay >= 1 && st.wDayOfWeek <= 6);
    check("SystemTimeToFileTime close to now", SystemTimeToFileTime(&st, &ft) &&
          (*(ULONGLONG *)&now - *(ULONGLONG *)&ft) < 50000000ULL);
    SYSTEMTIME fixed = {2000, 2, 0, 29, 12, 34, 56, 789};
    check("SystemTimeToFileTime leap day", SystemTimeToFileTime(&fixed, &ft) && *(ULONGLONG *)&ft == 125963012967890000ULL);
    SYSTEMTIME bad = {2001, 2, 0, 29, 0, 0, 0, 0};
    SetLastError(0);
    check("invalid SYSTEMTIME rejected", !SystemTimeToFileTime(&bad, &ft) && GetLastError() == ERROR_INVALID_PARAMETER);
    ULONGLONG t1 = 0, t2 = 0;
    QueryUnbiasedInterruptTime(&t1); Sleep(20); QueryUnbiasedInterruptTime(&t2);
    check("unbiased interrupt time advances", t2 > t1 && t2 - t1 >= 150000);
    FILETIME c, e, k, u;
    check("GetProcessTimes", GetProcessTimes(GetCurrentProcess(), &c, &e, &k, &u) && *(ULONGLONG *)&c > 0 && *(ULONGLONG *)&c <= *(ULONGLONG *)&now + 10000000ULL);

    /* Version verification. */
    OSVERSIONINFOEXW v = {0}; v.dwOSVersionInfoSize = sizeof v; v.dwMajorVersion = 6;
    ULONGLONG m = VerSetConditionMask(0, VER_MAJORVERSION, VER_GREATER_EQUAL);
    check("VerSetConditionMask places the condition", m == (0x8000000000000000ULL | (ULONGLONG)VER_GREATER_EQUAL << 3));
    check("running at least Windows 6", VerifyVersionInfoW(&v, VER_MAJORVERSION, m));
    v.dwMajorVersion = 99;
    SetLastError(0);
    check("not Windows 99", !VerifyVersionInfoW(&v, VER_MAJORVERSION, m) && GetLastError() == ERROR_OLD_WIN_VERSION);

    /* Pointer probes. */
    BYTE *page = VirtualAlloc(NULL, 4096, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
    check("IsBadReadPtr on committed memory", page && !IsBadReadPtr(page, 4096));
    check("IsBadReadPtr NULL", IsBadReadPtr(NULL, 1));
    VirtualFree(page, 0, MEM_RELEASE);
    check("IsBadReadPtr after release", IsBadReadPtr(page, 1));

    /* SearchPath for this program, then a missing file. */
    WCHAR self[MAX_PATH], found[MAX_PATH]; WCHAR *part = NULL;
    DWORD len = GetModuleFileNameW(NULL, self, MAX_PATH);
    WCHAR *leaf = self + len; while (leaf > self && leaf[-1] != '\\') leaf--;
    DWORD r = SearchPathW(NULL, leaf, NULL, MAX_PATH, found, &part);
    check("SearchPathW finds the program", r > 0 && r < MAX_PATH && part && wstr_eq(part, leaf));
    SetLastError(0);
    check("SearchPathW missing file", SearchPathW(NULL, L"no-such-osyx.dll", NULL, MAX_PATH, found, NULL) == 0 && GetLastError() == ERROR_FILE_NOT_FOUND);

    /* FormatMessage. */
    char msg[256];
    /* Windows answers in its installed UI language; OSyx has English text. */
    char marker[8]; int native = GetEnvironmentVariableA("OSYX", marker, sizeof marker) > 0;
    DWORD fm = FormatMessageA(FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS, NULL, ERROR_FILE_NOT_FOUND, 0, msg, sizeof msg, NULL);
    check("FormatMessageA system text", fm > 10 && msg[fm - 2] == '\r' && msg[fm - 1] == '\n' && (!native || mem_eq(msg, "The system cannot find the file specified.", 42)));
    DWORD_PTR args[] = {(DWORD_PTR)"zen", 42};
    fm = FormatMessageA(FORMAT_MESSAGE_FROM_STRING | FORMAT_MESSAGE_ARGUMENT_ARRAY, "%1 has %2!d! tabs%%", 0, 0, msg, sizeof msg, (va_list *)args);
    check("FormatMessageA inserts", fm == 16 && mem_eq(msg, "zen has 42 tabs%", 17));

    /* Registry round trip under HKCU, cleaned up afterwards. */
    HKEY key; DWORD disp = 0;
    LSTATUS s = RegCreateKeyExW(HKEY_CURRENT_USER, L"Software\\OSyxRuntimeTest\\Sub", 0, NULL, 0, KEY_ALL_ACCESS, NULL, &key, &disp);
    check_eq("RegCreateKeyExW status", s, ERROR_SUCCESS);
    check("RegCreateKeyExW creates", disp == REG_CREATED_NEW_KEY);
    DWORD dw = 0x1234, type = 0, size = sizeof dw;
    check("RegSetValueExW DWORD", RegSetValueExW(key, L"Count", 0, REG_DWORD, (BYTE *)&dw, 4) == ERROR_SUCCESS);
    check("RegSetValueExW string", RegSetValueExW(key, L"Name", 0, REG_SZ, (BYTE *)L"Zen", 8) == ERROR_SUCCESS);
    check("RegSetValueExW expand string", RegSetValueExW(key, L"Dir", 0, REG_EXPAND_SZ, (BYTE *)L"%OS%\\x", 14) == ERROR_SUCCESS);
    RegCloseKey(key);
    check("RegOpenKeyExW existing", RegOpenKeyExW(HKEY_CURRENT_USER, L"software\\osyxruntimetest\\SUB", 0, KEY_READ, &key) == ERROR_SUCCESS);
    dw = 0;
    check("RegQueryValueExW DWORD", RegQueryValueExW(key, L"count", NULL, &type, (BYTE *)&dw, &size) == ERROR_SUCCESS && type == REG_DWORD && dw == 0x1234);
    WCHAR text[32]; size = 2;
    check("RegQueryValueExW too small", RegQueryValueExW(key, L"Name", NULL, &type, (BYTE *)text, &size) == ERROR_MORE_DATA && size == 8);
    size = sizeof text;
    check("RegGetValueW string", RegGetValueW(HKEY_CURRENT_USER, L"Software\\OSyxRuntimeTest\\Sub", L"Name", RRF_RT_REG_SZ, &type, text, &size) == ERROR_SUCCESS && wstr_eq(text, L"Zen"));
    size = sizeof text;
    check("RegGetValueW expands", RegGetValueW(key, NULL, L"Dir", RRF_RT_REG_SZ, &type, text, &size) == ERROR_SUCCESS && type == REG_SZ && wstr_eq(text, L"Windows_NT\\x"));
    size = sizeof text;
    check("RegGetValueW type filter", RegGetValueW(key, NULL, L"Name", RRF_RT_REG_DWORD, &type, text, &size) == ERROR_UNSUPPORTED_TYPE);
    check("missing value", RegQueryValueExW(key, L"Nope", NULL, NULL, NULL, NULL) == ERROR_FILE_NOT_FOUND);
    check("read-only handle cannot write", RegSetValueExW(key, L"X", 0, REG_DWORD, (BYTE *)&dw, 4) == ERROR_ACCESS_DENIED);
    RegCloseKey(key);
    check("missing key", RegOpenKeyExW(HKEY_CURRENT_USER, L"Software\\OSyxRuntimeTest\\None", 0, KEY_READ, &key) == ERROR_FILE_NOT_FOUND);
    check("key with subkeys is not deleted", RegDeleteKeyW(HKEY_CURRENT_USER, L"Software\\OSyxRuntimeTest") != ERROR_SUCCESS);
    check("delete subkey", RegDeleteKeyW(HKEY_CURRENT_USER, L"Software\\OSyxRuntimeTest\\Sub") == ERROR_SUCCESS);
    check("delete parent", RegDeleteKeyW(HKEY_CURRENT_USER, L"Software\\OSyxRuntimeTest") == ERROR_SUCCESS);
    finish("runtime");
}
