/* kernel32 imports of xul.dll: time zones and calendar time, en-US
 * locale data and formatting, lstr* strings, global memory, named object
 * opening, memory locks, cancellation, computer names, processor and
 * memory information, power requests, restart registration and file
 * names. Locale-dependent results use the explicit en-US locale. */
#include "common.h"
#include <winnls.h>
#include <psapi.h>

static int wstr_eq(const WCHAR *a, const WCHAR *b) {
    while (*a && *a == *b) a++, b++;
    return *a == *b;
}

static int str_eq(const char *a, const char *b) {
    while (*a && *a == *b) a++, b++;
    return *a == *b;
}

static int wstarts(const WCHAR *s, const WCHAR *p) {
    while (*p && *s == *p) s++, p++;
    return *p == 0;
}

static int wends(const WCHAR *s, const WCHAR *tail) {
    int n = 0, t = 0;
    while (s[n]) n++;
    while (tail[t]) t++;
    return n >= t && wstr_eq(s + n - t, tail);
}

static void wappend(WCHAR *d, const WCHAR *s) {
    while (*d) d++;
    while ((*d++ = *s++)) {}
}

typedef BOOL(WINAPI *Pinfo)(PSYSTEM_LOGICAL_PROCESSOR_INFORMATION, PDWORD);

static CONDITION_VARIABLE cv;
static CRITICAL_SECTION cs;

static DWORD WINAPI copied(LARGE_INTEGER a, LARGE_INTEGER b, LARGE_INTEGER c, LARGE_INTEGER d, DWORD e, DWORD f, HANDLE g, HANDLE h, LPVOID data) {
    (void)a, (void)b, (void)c, (void)d, (void)e, (void)f, (void)g, (void)h;
    ++*(int *)data;
    return PROGRESS_CONTINUE;
}

void __stdcall start(void) {
    /* Calendar time and the zone */
    SYSTEMTIME st, local, viaTz;
    FILETIME ft = {0x5A5A0000, 0x01D9D4B3}; /* some instant in 2023 */
    check("FileTimeToSystemTime", FileTimeToSystemTime(&ft, &st) && st.wYear == 2023);
    FILETIME back;
    check("... and back", SystemTimeToFileTime(&st, &back) && back.dwHighDateTime == ft.dwHighDateTime &&
                              back.dwLowDateTime / 10000 == ft.dwLowDateTime / 10000);
    FILETIME later = ft;
    later.dwHighDateTime++;
    check("CompareFileTime", CompareFileTime(&ft, &later) == -1 && CompareFileTime(&later, &ft) == 1 && CompareFileTime(&ft, &ft) == 0);
    TIME_ZONE_INFORMATION tzi;
    DWORD id = GetTimeZoneInformation(&tzi);
    check("GetTimeZoneInformation", id != TIME_ZONE_ID_INVALID && tzi.Bias > -900 && tzi.Bias < 900);
    DYNAMIC_TIME_ZONE_INFORMATION dtzi;
    check("GetDynamicTimeZoneInformation agrees", GetDynamicTimeZoneInformation(&dtzi) == id && dtzi.Bias == tzi.Bias);
    TIME_ZONE_INFORMATION year;
    check("GetTimeZoneInformationForYear", GetTimeZoneInformationForYear(2024, NULL, &year) && year.Bias == tzi.Bias);
    GetSystemTime(&st);
    GetLocalTime(&local);
    check("SystemTimeToTzSpecificLocalTime matches GetLocalTime",
          SystemTimeToTzSpecificLocalTime(NULL, &st, &viaTz) && viaTz.wHour == local.wHour && viaTz.wMinute == local.wMinute);
    TIME_ZONE_INFORMATION cet = {0};
    cet.Bias = -60;
    cet.DaylightBias = -60;
    cet.StandardDate.wMonth = 10, cet.StandardDate.wDay = 5, cet.StandardDate.wHour = 3;
    cet.DaylightDate.wMonth = 3, cet.DaylightDate.wDay = 5, cet.DaylightDate.wHour = 2;
    SYSTEMTIME summer = {2024, 7, 0, 1, 12, 0, 0, 0}, winter = {2024, 1, 0, 1, 12, 0, 0, 0};
    check("CET rules: 12:00 UTC is 14:00 in July", SystemTimeToTzSpecificLocalTime(&cet, &summer, &viaTz) && viaTz.wHour == 14);
    check("... and 13:00 in January", SystemTimeToTzSpecificLocalTime(&cet, &winter, &viaTz) && viaTz.wHour == 13);

    /* Locale identifiers */
    WCHAR w[128];
    check("GetLocaleInfoW(0x0409, LOCALE_SNAME)", GetLocaleInfoW(0x0409, LOCALE_SNAME, w, 128) == 6 && wstr_eq(w, L"en-US"));
    check("LCIDToLocaleName", LCIDToLocaleName(0x0409, w, 128, 0) == 6 && wstr_eq(w, L"en-US"));
    check("LocaleNameToLCID", LocaleNameToLCID(L"en-US", 0) == 0x0409);
    check("ResolveLocaleName(en)", ResolveLocaleName(L"en", w, 128) > 0 && wstr_eq(w, L"en-US"));
    check("CompareStringOrdinal ignoring case", CompareStringOrdinal(L"abc", -1, L"ABD", -1, TRUE) == CSTR_LESS_THAN &&
                                                   CompareStringOrdinal(L"abc", -1, L"ABC", -1, TRUE) == CSTR_EQUAL &&
                                                   CompareStringOrdinal(L"b", 1, L"B", 1, FALSE) == CSTR_GREATER_THAN);
    check("GetGeoInfoW(US, GEO_ISO2)", GetGeoInfoW(244, GEO_ISO2, w, 128, 0) == 3 && wstr_eq(w, L"US"));
    ULONG langs = 0, size = 0;
    check("GetUserPreferredUILanguages size query", GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &langs, NULL, &size) && langs >= 1 && size > 2);

    /* Formatting in en-US */
    SYSTEMTIME d = {2024, 3, 0, 5, 14, 7, 9, 0};
    check("GetDateFormatEx short", GetDateFormatEx(L"en-US", DATE_SHORTDATE, &d, NULL, w, 128, NULL) && wstr_eq(w, L"3/5/2024"));
    check("GetDateFormatEx long", GetDateFormatEx(L"en-US", DATE_LONGDATE, &d, NULL, w, 128, NULL) && wstr_eq(w, L"Tuesday, March 5, 2024"));
    check("GetDateFormatEx picture", GetDateFormatEx(L"en-US", 0, &d, L"ddd',' dd MMM yy", w, 128, NULL) && wstr_eq(w, L"Tue, 05 Mar 24"));
    char a[64];
    check("GetDateFormatA", GetDateFormatA(0x0409, 0, &d, "yyyy-MM-dd", a, 64) == 11 && str_eq(a, "2024-03-05"));
    check("GetTimeFormatEx", GetTimeFormatEx(L"en-US", 0, &d, NULL, w, 128) && wstr_eq(w, L"2:07:09 PM"));
    check("GetTimeFormatEx without seconds", GetTimeFormatEx(L"en-US", TIME_NOSECONDS, &d, NULL, w, 128) && wstr_eq(w, L"2:07 PM"));
    check("GetTimeFormatA picture", GetTimeFormatA(0x0409, 0, &d, "HH'h'mm", a, 64) && str_eq(a, "14h07"));
    int need = GetDateFormatEx(L"en-US", DATE_LONGDATE, &d, NULL, NULL, 0, NULL);
    check("a size query counts the NUL", need == 23);
    check("GetNumberFormatEx", GetNumberFormatEx(L"en-US", 0, L"-1234567.895", NULL, w, 128) && wstr_eq(w, L"-1,234,567.90"));
    NUMBERFMTW nf = {1, 0, 32, L",", L".", 0};
    check("GetNumberFormatEx with NUMBERFMT", GetNumberFormatEx(L"en-US", 0, L"-1234567.25", &nf, w, 128) && wstr_eq(w, L"(12.34.567,3)"));
    nf.LeadingZero = 0, nf.NumDigits = 2, nf.Grouping = 0;
    check("... without a leading zero", GetNumberFormatEx(L"en-US", 0, L"0.5", &nf, w, 128) && wstr_eq(w, L",50"));
    check("an invalid number fails", !GetNumberFormatEx(L"en-US", 0, L"1e5", NULL, w, 128) && GetLastError() == ERROR_INVALID_PARAMETER);
    CURRENCYFMTW cf = {2, 1, 3, L".", L",", 1, 0, L"$"};
    check("GetCurrencyFormatEx with CURRENCYFMT", GetCurrencyFormatEx(L"en-US", 0, L"-1234.5", &cf, w, 128) && wstr_eq(w, L"-$1,234.50"));
    cf.NegativeOrder = 0;
    check("... negative order 0", GetCurrencyFormatEx(L"en-US", 0, L"-1234.5", &cf, w, 128) && wstr_eq(w, L"($1,234.50)"));

    /* lstr* */
    WCHAR buf[32];
    lstrcpyW(buf, L"Hello");
    lstrcatW(buf, L", OSyx");
    check("lstrcpyW + lstrcatW", wstr_eq(buf, L"Hello, OSyx"));
    check("lstrcpynW truncates", wstr_eq(lstrcpynW(buf, L"abcdef", 4), L"abc"));
    check("lstrcmpiW", lstrcmpiW(L"abc", L"ABC") == 0 && lstrcmpiW(L"abc", L"abd") < 0);

    /* Global memory */
    HGLOBAL g = GlobalAlloc(GMEM_FIXED | GMEM_ZEROINIT, 100);
    check("GlobalSize", GlobalSize(g) == 100);
    check("GlobalLock of a fixed block", GlobalLock(g) == g);
    check("GlobalUnlock of a fixed block", GlobalUnlock(g));
    HGLOBAL g2 = GlobalReAlloc(g, 4000, GMEM_MOVEABLE | GMEM_ZEROINIT);
    check("GlobalReAlloc grows a fixed block", g2 != NULL && GlobalSize(g2) >= 4000);
    GlobalFree(g2);

    /* Named objects */
    HANDLE ev = CreateEventA(NULL, TRUE, FALSE, "k32xul-event");
    HANDLE ev2 = OpenEventA(EVENT_ALL_ACCESS, FALSE, "k32xul-event");
    check("OpenEventA", ev2 != NULL && SetEvent(ev2) && WaitForSingleObject(ev, 0) == WAIT_OBJECT_0);
    check("OpenEventA of an unknown name", OpenEventA(EVENT_ALL_ACCESS, FALSE, "k32xul-none") == NULL && GetLastError() == ERROR_FILE_NOT_FOUND);
    HANDLE mx = CreateMutexW(NULL, FALSE, L"k32xul-mutex");
    HANDLE mx2 = OpenMutexW(SYNCHRONIZE, FALSE, L"k32xul-mutex");
    check("OpenMutexW", mx2 != NULL && WaitForSingleObject(mx2, 0) == WAIT_OBJECT_0 && ReleaseMutex(mx2));
    HANDLE sec = CreateFileMappingW(INVALID_HANDLE_VALUE, NULL, PAGE_READWRITE, 0, 4096, L"k32xul-section");
    HANDLE sec2 = OpenFileMappingW(FILE_MAP_ALL_ACCESS, FALSE, L"k32xul-section");
    char *v1 = MapViewOfFileEx(sec, FILE_MAP_ALL_ACCESS, 0, 0, 4096, NULL);
    char *v2 = MapViewOfFile(sec2, FILE_MAP_ALL_ACCESS, 0, 0, 4096);
    check("OpenFileMappingW shares the section", v1 && v2 && (v1[7] = 42, v2[7] == 42));
    UnmapViewOfFile(v1), UnmapViewOfFile(v2);
    CloseHandle(ev), CloseHandle(ev2), CloseHandle(mx), CloseHandle(mx2), CloseHandle(sec), CloseHandle(sec2);

    InitializeConditionVariable(&cv);
    InitializeCriticalSection(&cs);
    EnterCriticalSection(&cs);
    check("SleepConditionVariableCS times out", !SleepConditionVariableCS(&cv, &cs, 20) && GetLastError() == ERROR_TIMEOUT);
    LeaveCriticalSection(&cs);

    void *mem = VirtualAlloc(NULL, 8192, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
    check("VirtualLock + VirtualUnlock", VirtualLock(mem, 8192) && VirtualUnlock(mem, 8192));
    check("VirtualUnlock of an unlocked range", !VirtualUnlock(mem, 8192) && GetLastError() == ERROR_NOT_LOCKED);
    VirtualFree(mem, 0, MEM_RELEASE);
    MEMORY_BASIC_INFORMATION mbi = {0};
    check("VirtualQueryEx of this process", VirtualQueryEx(GetCurrentProcess(), &mbi, &mbi, sizeof mbi) == sizeof mbi);

    /* Files */
    WCHAR tmp[MAX_PATH], other[MAX_PATH], fin[MAX_PATH];
    GetTempPathW(MAX_PATH, tmp);
    lstrcpyW(other, tmp);
    wappend(tmp, L"k32xul.tmp");
    wappend(other, L"k32xul-copy.tmp");
    HANDLE f = CreateFileW(tmp, GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_DELETE, NULL, CREATE_ALWAYS, 0, NULL);
    DWORD n;
    WriteFile(f, "data", 4, &n, NULL);
    check("CancelIoEx with nothing pending", !CancelIoEx(f, NULL) && GetLastError() == ERROR_NOT_FOUND);
    DWORD fl = GetFinalPathNameByHandleW(f, fin, MAX_PATH, 0);
    check("GetFinalPathNameByHandleW", fl > 0 && wstarts(fin, L"\\\\?\\") && wends(fin, L"k32xul.tmp"));
    check("GetLongPathNameW of an existing file", GetLongPathNameW(tmp, fin, MAX_PATH) > 0 && wends(fin, L"k32xul.tmp"));
    CloseHandle(f);
    int calls = 0;
    check("CopyFileExW calls the progress routine", CopyFileExW(tmp, other, copied, &calls, NULL, 0) && calls >= 1);
    check("CopyFileExW refuses an existing target", !CopyFileExW(tmp, other, NULL, NULL, NULL, COPY_FILE_FAIL_IF_EXISTS) && GetLastError() == ERROR_FILE_EXISTS);
    char tmpa[MAX_PATH], othera[MAX_PATH];
    WideCharToMultiByte(CP_UTF8, 0, tmp, -1, tmpa, MAX_PATH, NULL, NULL);
    WideCharToMultiByte(CP_UTF8, 0, other, -1, othera, MAX_PATH, NULL, NULL);
    check("ReplaceFileA", ReplaceFileA(tmpa, othera, NULL, 0, NULL, NULL) && GetFileAttributesW(other) == INVALID_FILE_ATTRIBUTES);
    DeleteFileW(tmp);
    check("QueryDosDeviceW(C:)", QueryDosDeviceW(L"C:", fin, MAX_PATH) > 0 && wstarts(fin, L"\\Device\\"));
    check("GetVolumePathNameW", GetVolumePathNameW(tmp, fin, MAX_PATH) && wstr_eq(fin, L"C:\\"));
    check("GetSystemWindowsDirectoryW", GetSystemWindowsDirectoryW(fin, MAX_PATH) > 2);

    /* Machine and process */
    DWORD_PTR process_mask = 0, system_mask = 0;
    check("GetProcessAffinityMask", GetProcessAffinityMask(GetCurrentProcess(), &process_mask, &system_mask) &&
          process_mask != 0 && (process_mask & system_mask) == process_mask);
    check("GetProcessAffinityMask invalid handle", !GetProcessAffinityMask(NULL, &process_mask, &system_mask) &&
          GetLastError() == ERROR_INVALID_HANDLE);
    DWORD cn = 0;
    check("GetComputerNameW size query", !GetComputerNameW(NULL, &cn) && GetLastError() == ERROR_BUFFER_OVERFLOW && cn > 1);
    cn = 64;
    check("GetComputerNameW", GetComputerNameW(w, &cn) && cn > 0 && w[cn] == 0);
    DWORD an = 64;
    check("GetComputerNameExA(DNS host name)", GetComputerNameExA(ComputerNameDnsHostname, a, &an) && an > 0);
    DWORD len = 0;
    check("GetLogicalProcessorInformation size query", !GetLogicalProcessorInformation(NULL, &len) && GetLastError() == ERROR_INSUFFICIENT_BUFFER && len >= 32);
    SYSTEM_LOGICAL_PROCESSOR_INFORMATION lpi[64];
    len = sizeof lpi;
    int cores = 0;
    if (GetLogicalProcessorInformation(lpi, &len))
        for (DWORD i = 0; i < len / sizeof lpi[0]; i++) cores += lpi[i].Relationship == RelationProcessorCore;
    check("... lists a processor core", cores >= 1);
    GROUP_AFFINITY ga;
    check("GetThreadGroupAffinity", GetThreadGroupAffinity(GetCurrentThread(), &ga) && ga.Mask != 0);
    PROCESS_MEMORY_COUNTERS pmc;
    check("K32GetProcessMemoryInfo", K32GetProcessMemoryInfo(GetCurrentProcess(), &pmc, sizeof pmc) && pmc.WorkingSetSize > 0 &&
                                         pmc.PeakWorkingSetSize >= pmc.WorkingSetSize);
    DWORD sid = 99;
    check("ProcessIdToSessionId", ProcessIdToSessionId(GetCurrentProcessId(), &sid) && sid != 99);
    check("GetProcessIdOfThread", GetProcessIdOfThread(GetCurrentThread()) == GetCurrentProcessId());
    UINT prev = SetErrorMode(SEM_FAILCRITICALERRORS);
    check("SetErrorMode returns the previous mode", SetErrorMode(prev) == SEM_FAILCRITICALERRORS);
    DWORD oldm = 7;
    check("SetThreadErrorMode", SetThreadErrorMode(SEM_FAILCRITICALERRORS, &oldm) && SetThreadErrorMode(oldm, NULL));
    ULONG guarantee = 16384;
    check("SetThreadStackGuarantee", SetThreadStackGuarantee(&guarantee));
    REASON_CONTEXT rc = {POWER_REQUEST_CONTEXT_VERSION, POWER_REQUEST_CONTEXT_SIMPLE_STRING, {{0}}};
    rc.Reason.SimpleReasonString = L"k32xul";
    HANDLE pr = PowerCreateRequest(&rc);
    check("power request", pr != INVALID_HANDLE_VALUE && PowerSetRequest(pr, PowerRequestDisplayRequired) &&
                               PowerClearRequest(pr, PowerRequestDisplayRequired) && CloseHandle(pr));
    check("RegisterApplicationRestart", RegisterApplicationRestart(L"--restarted", 0) == S_OK);
    DWORD rsize = 64, rflags = 9;
    check("GetApplicationRestartSettings", GetApplicationRestartSettings(GetCurrentProcess(), w, &rsize, &rflags) == S_OK &&
                                              wstr_eq(w, L"--restarted") && rflags == 0);
    check("UnregisterApplicationRestart", UnregisterApplicationRestart() == S_OK);
    HANDLE low = CreateMemoryResourceNotification(LowMemoryResourceNotification);
    check("CreateMemoryResourceNotification", low != NULL && CloseHandle(low));
    finish("k32xul");
}
