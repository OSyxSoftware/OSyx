/* The newer Win32 surface msvcp140 and zen.exe import: CreateFile2,
 * CopyFile2 and handle information, NLS for an explicit locale, the
 * default thread pool and completion ports, child processes and job
 * objects, self "remote" memory, shlwapi paths and WinRT strings.
 * The same PE runs on Windows. */
#include "common.h"
#include <shlwapi.h>
#include <winstring.h>

HRESULT WINAPI RoGetActivationFactory(HSTRING, REFIID, void **);

static int wide_eq(const WCHAR *a, const WCHAR *b) {
    while (*a && *a == *b) a++, b++;
    return *a == *b;
}

static int wide_ends(const WCHAR *s, const WCHAR *tail) {
    DWORD n = 0, m = 0;
    while (s[n]) n++;
    while (tail[m]) m++;
    return n >= m && wide_eq(s + n - m, tail);
}

static void wide_cat(WCHAR *d, const WCHAR *s) {
    while (*d) d++;
    while ((*d++ = *s++)) {}
}

static void temp_name(WCHAR *out, const WCHAR *leaf) {
    out[GetTempPathW(MAX_PATH, out)] = 0;
    wide_cat(out, leaf);
}

static void files(void) {
    WCHAR a[MAX_PATH], b[MAX_PATH];
    temp_name(a, L"osyx-platform-a.bin");
    temp_name(b, L"osyx-platform-b.bin");
    DeleteFileW(a);
    DeleteFileW(b);
    check("AreFileApisANSI", AreFileApisANSI());

    CREATEFILE2_EXTENDED_PARAMETERS ext = {sizeof ext, FILE_ATTRIBUTE_NORMAL, 0, 0, NULL, NULL};
    HANDLE f = CreateFile2(a, GENERIC_READ | GENERIC_WRITE, 0, CREATE_NEW, &ext);
    check("CreateFile2 CREATE_NEW", f != INVALID_HANDLE_VALUE);
    DWORD w = 0;
    check("write through a CreateFile2 handle", WriteFile(f, "0123456789abcdef", 16, &w, NULL) && w == 16);
    SetLastError(0);
    check("CreateFile2 CREATE_NEW on an existing file",
          CreateFile2(a, GENERIC_READ, FILE_SHARE_READ, CREATE_NEW, NULL) == INVALID_HANDLE_VALUE && GetLastError() == ERROR_FILE_EXISTS);

    FILE_STANDARD_INFO std;
    check("FileStandardInfo: size, not a directory",
          GetFileInformationByHandleEx(f, FileStandardInfo, &std, sizeof std) && std.EndOfFile.QuadPart == 16 && !std.Directory);
    FILE_BASIC_INFO basic;
    check("FileBasicInfo: a file with times",
          GetFileInformationByHandleEx(f, FileBasicInfo, &basic, sizeof basic) &&
              !(basic.FileAttributes & FILE_ATTRIBUTE_DIRECTORY) && basic.LastWriteTime.QuadPart > 0);
    struct { DWORD len; WCHAR name[MAX_PATH]; } nameinfo;
    int got = GetFileInformationByHandleEx(f, FileNameInfo, &nameinfo, sizeof nameinfo);
    if (got) nameinfo.name[nameinfo.len / 2] = 0;
    check("FileNameInfo: path without the drive", got && nameinfo.name[0] == L'\\' && wide_ends(nameinfo.name, L"\\osyx-platform-a.bin"));
    SetLastError(0);
    check("too small a buffer is ERROR_BAD_LENGTH",
          !GetFileInformationByHandleEx(f, FileStandardInfo, &std, 8) && GetLastError() == ERROR_BAD_LENGTH);

    FILE_END_OF_FILE_INFO eof = {{.QuadPart = 10}};
    check("FileEndOfFileInfo truncates", SetFileInformationByHandle(f, FileEndOfFileInfo, &eof, sizeof eof));
    check("SetFileTime with no times", SetFileTime(f, NULL, NULL, NULL));
    check("close", CloseHandle(f));

    WIN32_FILE_ATTRIBUTE_DATA data;
    check("GetFileAttributesExW: truncated size",
          GetFileAttributesExW(a, GetFileExInfoStandard, &data) && data.nFileSizeLow == 10 && data.nFileSizeHigh == 0);
    SetLastError(0);
    check("GetFileAttributesExW of a missing file",
          !GetFileAttributesExW(b, GetFileExInfoStandard, &data) && GetLastError() == ERROR_FILE_NOT_FOUND);

    check("SetFileAttributesW read-only", SetFileAttributesW(a, FILE_ATTRIBUTE_READONLY));
    check("read-only attribute reported", (GetFileAttributesW(a) & FILE_ATTRIBUTE_READONLY) != 0);
    SetLastError(0);
    check("read-only file refuses write access",
          CreateFile2(a, GENERIC_WRITE, 0, OPEN_EXISTING, NULL) == INVALID_HANDLE_VALUE && GetLastError() == ERROR_ACCESS_DENIED);
    check("SetFileAttributesW normal", SetFileAttributesW(a, FILE_ATTRIBUTE_NORMAL));
    check("read-only attribute cleared", !(GetFileAttributesW(a) & FILE_ATTRIBUTE_READONLY));

    check("CopyFile2", CopyFile2(a, b, NULL) == S_OK);
    check("copy has the data", GetFileAttributesExW(b, GetFileExInfoStandard, &data) && data.nFileSizeLow == 10);
    COPYFILE2_EXTENDED_PARAMETERS cp = {sizeof cp, COPY_FILE_FAIL_IF_EXISTS, NULL, NULL, NULL};
    check("CopyFile2 fail-if-exists", CopyFile2(a, b, &cp) == HRESULT_FROM_WIN32(ERROR_FILE_EXISTS));
    SetLastError(0);
    check("CopyFileW fail-if-exists", !CopyFileW(a, b, TRUE) && GetLastError() == ERROR_FILE_EXISTS);

    WCHAR pattern[MAX_PATH];
    temp_name(pattern, L"osyx-platform-*.bin");
    WIN32_FIND_DATAW fd;
    HANDLE find = FindFirstFileExW(pattern, FindExInfoBasic, &fd, FindExSearchNameMatch, NULL, FIND_FIRST_EX_LARGE_FETCH);
    int found = 0;
    if (find != INVALID_HANDLE_VALUE) {
        do found += wide_ends(fd.cFileName, L".bin");
        while (FindNextFileW(find, &fd));
        FindClose(find);
    }
    check_eq("FindFirstFileExW (basic, large fetch) finds both files", found, 2);

    /* Delete on close through FileDispositionInfo. */
    HANDLE d = CreateFile2(b, GENERIC_READ | DELETE, FILE_SHARE_READ | FILE_SHARE_DELETE, OPEN_EXISTING, NULL);
    FILE_DISPOSITION_INFO del = {TRUE};
    check("FileDispositionInfo", d != INVALID_HANDLE_VALUE && SetFileInformationByHandle(d, FileDispositionInfo, &del, sizeof del));
    CloseHandle(d);
    check("disposition deleted the file on close", GetFileAttributesW(b) == INVALID_FILE_ATTRIBUTES);
    check("delete the first file", DeleteFileW(a));
}

static void nls(void) {
    WCHAR buf[64];
    check("GetLocaleInfoEx LOCALE_SNAME", GetLocaleInfoEx(L"en-US", LOCALE_SNAME, buf, 64) == 6 && wide_eq(buf, L"en-US"));
    check("GetLocaleInfoEx English language name",
          GetLocaleInfoEx(L"en-US", LOCALE_SENGLISHLANGUAGENAME | LOCALE_NOUSEROVERRIDE, buf, 64) && wide_eq(buf, L"English"));
    check("GetLocaleInfoEx decimal separator", GetLocaleInfoEx(L"en-US", LOCALE_SDECIMAL | LOCALE_NOUSEROVERRIDE, buf, 64) == 2 && wide_eq(buf, L"."));
    check("GetLocaleInfoEx first day name", GetLocaleInfoEx(L"en-US", LOCALE_SDAYNAME1, buf, 64) && wide_eq(buf, L"Monday"));
    check("GetLocaleInfoEx abbreviated month", GetLocaleInfoEx(L"en-US", LOCALE_SABBREVMONTHNAME1, buf, 64) && wide_eq(buf, L"Jan"));
    DWORD lang = 0;
    check_eq("GetLocaleInfoEx LOCALE_ILANGUAGE as a number",
             GetLocaleInfoEx(L"en-US", LOCALE_ILANGUAGE | LOCALE_RETURN_NUMBER, (WCHAR *)&lang, 2) == 2 ? lang : 0, 0x409);
    check("GetLocaleInfoEx size query", GetLocaleInfoEx(L"en-US", LOCALE_SNAME, NULL, 0) == 6);
    check("GetUserDefaultLocaleName", GetUserDefaultLocaleName(buf, 64) > 1);
    check("user default LCID and language agree", LANGIDFROMLCID(GetUserDefaultLCID()) == GetUserDefaultLangID());

    check_eq("CompareStringEx orders words", CompareStringEx(L"en-US", 0, L"apple", -1, L"banana", -1, NULL, NULL, 0), CSTR_LESS_THAN);
    check_eq("CompareStringEx: lower case first", CompareStringEx(L"en-US", 0, L"a", -1, L"A", -1, NULL, NULL, 0), CSTR_LESS_THAN);
    check_eq("CompareStringEx ignoring case", CompareStringEx(L"en-US", NORM_IGNORECASE, L"ABC", 3, L"abc", 3, NULL, NULL, 0), CSTR_EQUAL);
    check_eq("CompareStringEx: letters before case", CompareStringEx(L"en-US", 0, L"Ab", -1, L"ac", -1, NULL, NULL, 0), CSTR_LESS_THAN);

    check("LCMapStringEx upper case", LCMapStringEx(L"en-US", LCMAP_UPPERCASE, L"hello", -1, buf, 64, NULL, NULL, 0) == 6 && wide_eq(buf, L"HELLO"));
    check("LCMapStringEx lower case, counted", LCMapStringEx(L"en-US", LCMAP_LOWERCASE, L"MiXeD", 5, buf, 64, NULL, NULL, 0) == 5 &&
                                                   buf[0] == L'm' && buf[4] == L'd');
    BYTE k1[64], k2[64], k3[64];
    int n1 = LCMapStringEx(L"en-US", LCMAP_SORTKEY, L"apple", -1, (WCHAR *)k1, sizeof k1, NULL, NULL, 0);
    int n2 = LCMapStringEx(L"en-US", LCMAP_SORTKEY, L"banana", -1, (WCHAR *)k2, sizeof k2, NULL, NULL, 0);
    int n3 = LCMapStringEx(L"en-US", LCMAP_SORTKEY, L"Apple", -1, (WCHAR *)k3, sizeof k3, NULL, NULL, 0);
    int c12 = 0, c13 = 0;
    for (int i = 0; n1 && n2 && !c12; i++) c12 = k1[i] - k2[i];
    for (int i = 0; n1 && n3 && !c13; i++) c13 = k1[i] - k3[i];
    check("sort keys: apple < banana", n1 > 0 && n2 > 0 && c12 < 0);
    check("sort keys: apple < Apple", n3 > 0 && c13 < 0);

    WORD types[3];
    check("GetStringTypeW CT_CTYPE1", GetStringTypeW(CT_CTYPE1, L"A1 ", 3, types) && (types[0] & (C1_UPPER | C1_ALPHA)) == (C1_UPPER | C1_ALPHA) &&
                                          (types[1] & C1_DIGIT) && (types[2] & (C1_SPACE | C1_BLANK)) == (C1_SPACE | C1_BLANK));
    CPINFO cpi;
    check("GetCPInfo(CP_UTF8)", GetCPInfo(CP_UTF8, &cpi) && cpi.MaxCharSize == 4);
}

static BOOL CALLBACK find_en_us(LPWSTR name, DWORD flags, LPARAM param) {
    (void)flags;
    if (wide_eq(name, L"en-US")) *(int *)param = 1;
    return TRUE;
}

static volatile LONG work_runs, timer_runs;
static HANDLE timer_done, wait_done, registered_done;
static volatile DWORD wait_result = 1234;
static volatile BOOLEAN registered_timed_out = 7;

static void CALLBACK work_cb(PTP_CALLBACK_INSTANCE i, void *ctx, PTP_WORK w) {
    (void)i, (void)w;
    InterlockedAdd((LONG *)ctx, 1);
}

static void CALLBACK timer_cb(PTP_CALLBACK_INSTANCE i, void *ctx, PTP_TIMER t) {
    (void)i, (void)ctx, (void)t;
    if (InterlockedIncrement(&timer_runs) == 3) SetEvent(timer_done);
}

static void CALLBACK wait_cb(PTP_CALLBACK_INSTANCE i, void *ctx, PTP_WAIT w, TP_WAIT_RESULT r) {
    (void)i, (void)ctx, (void)w;
    wait_result = r;
    SetEvent(wait_done);
}

static void CALLBACK registered_cb(void *ctx, BOOLEAN timed_out) {
    (void)ctx;
    registered_timed_out = timed_out;
    SetEvent(registered_done);
}

static void pool(void) {
    PTP_WORK work = CreateThreadpoolWork(work_cb, (void *)&work_runs, NULL);
    check("CreateThreadpoolWork", work != NULL);
    for (int i = 0; i < 5; i++) SubmitThreadpoolWork(work);
    WaitForThreadpoolWorkCallbacks(work, FALSE);
    check_eq("five submissions ran", work_runs, 5);
    CloseThreadpoolWork(work);

    timer_done = CreateEventW(NULL, TRUE, FALSE, NULL);
    PTP_TIMER timer = CreateThreadpoolTimer(timer_cb, NULL, NULL);
    FILETIME due;
    *(LONGLONG *)&due = -10 * 10000LL; /* 10 ms from now */
    SetThreadpoolTimer(timer, &due, 5, 0);
    check("timer is set", IsThreadpoolTimerSet(timer));
    check("periodic timer fired three times", WaitForSingleObject(timer_done, 5000) == WAIT_OBJECT_0);
    SetThreadpoolTimer(timer, NULL, 0, 0);
    WaitForThreadpoolTimerCallbacks(timer, TRUE);
    check("stopped timer is not set", !IsThreadpoolTimerSet(timer));
    LONG after = timer_runs;
    Sleep(30);
    check("stopped timer stays quiet", timer_runs == after);
    CloseThreadpoolTimer(timer);

    wait_done = CreateEventW(NULL, FALSE, FALSE, NULL);
    HANDLE ev = CreateEventW(NULL, FALSE, FALSE, NULL);
    PTP_WAIT wait = CreateThreadpoolWait(wait_cb, NULL, NULL);
    SetThreadpoolWait(wait, ev, NULL);
    SetEvent(ev);
    check("wait callback after the event", WaitForSingleObject(wait_done, 5000) == WAIT_OBJECT_0 && wait_result == WAIT_OBJECT_0);
    *(LONGLONG *)&due = -20 * 10000LL;
    SetThreadpoolWait(wait, ev, &due);
    check("wait callback on timeout", WaitForSingleObject(wait_done, 5000) == WAIT_OBJECT_0 && wait_result == WAIT_TIMEOUT);
    WaitForThreadpoolWaitCallbacks(wait, FALSE);
    CloseThreadpoolWait(wait);

    registered_done = CreateEventW(NULL, FALSE, FALSE, NULL);
    HANDLE reg = NULL;
    check("RegisterWaitForSingleObject", RegisterWaitForSingleObject(&reg, ev, registered_cb, NULL, INFINITE, WT_EXECUTEONLYONCE));
    SetEvent(ev);
    check("registered callback, signaled", WaitForSingleObject(registered_done, 5000) == WAIT_OBJECT_0 && registered_timed_out == FALSE);
    check("UnregisterWaitEx, waiting for callbacks", UnregisterWaitEx(reg, INVALID_HANDLE_VALUE));
    CloseHandle(ev);

    HANDLE port = CreateIoCompletionPort(INVALID_HANDLE_VALUE, NULL, 0, 0);
    check("completion port", port != NULL);
    DWORD bytes = 0;
    ULONG_PTR key = 0;
    OVERLAPPED *ov = (OVERLAPPED *)1;
    check("post a completion", PostQueuedCompletionStatus(port, 7, 42, (OVERLAPPED *)0x1230));
    check("dequeue it", GetQueuedCompletionStatus(port, &bytes, &key, &ov, 1000) && bytes == 7 && key == 42 && ov == (OVERLAPPED *)0x1230);
    SetLastError(0);
    check("empty port times out", !GetQueuedCompletionStatus(port, &bytes, &key, &ov, 10) && GetLastError() == WAIT_TIMEOUT && ov == NULL);
    CloseHandle(port);
}

static void processes(void) {
    WCHAR exe[MAX_PATH], child[MAX_PATH];
    DWORD n = GetModuleFileNameW(NULL, exe, MAX_PATH);
    for (DWORD i = 0; i <= n; i++) child[i] = exe[i];
    while (n && child[n - 1] != L'\\') n--;
    child[n] = 0;
    wide_cat(child, L"minimal.exe");

    STARTUPINFOW si = {sizeof si};
    PROCESS_INFORMATION pi;
    WCHAR cmd[MAX_PATH + 2] = L"\"";
    wide_cat(cmd, child);
    wide_cat(cmd, L"\"");
    BOOL created = CreateProcessW(NULL, cmd, NULL, NULL, FALSE, 0, NULL, NULL, &si, &pi);
    if (!created) {
        char narrow[MAX_PATH];
        WideCharToMultiByte(CP_UTF8, 0, cmd, -1, narrow, MAX_PATH, NULL, NULL);
        put("       ");
        put(narrow);
        put(": error ");
        put_uint(GetLastError());
        put("\r\n");
    }
    check("CreateProcessW", created);
    check("child process handle signals at exit", WaitForSingleObject(pi.hProcess, 10000) == WAIT_OBJECT_0);
    DWORD code = 0;
    check_eq("child exit code", GetExitCodeProcess(pi.hProcess, &code) ? code : 0, 42);
    check("GetProcessId of the child", GetProcessId(pi.hProcess) == pi.dwProcessId && pi.dwProcessId != GetCurrentProcessId());
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    SetLastError(0);
    check("a missing program", !CreateProcessW(L"C:\\osyx-no-such.exe", NULL, NULL, NULL, FALSE, 0, NULL, NULL, &si, &pi) &&
                                   GetLastError() == ERROR_FILE_NOT_FOUND);

    HANDLE job = CreateJobObjectW(NULL, NULL);
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION lim = {0};
    lim.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    check("job with kill-on-close", job && SetInformationJobObject(job, JobObjectExtendedLimitInformation, &lim, sizeof lim));
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION back = {0};
    DWORD len = 0;
    check("job limits read back", QueryInformationJobObject(job, JobObjectExtendedLimitInformation, &back, sizeof back, &len) &&
                                      back.BasicLimitInformation.LimitFlags == JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE);
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION acct;
    check("empty job accounting", QueryInformationJobObject(job, JobObjectBasicAccountingInformation, &acct, sizeof acct, NULL) &&
                                      acct.TotalProcesses == 0 && acct.ActiveProcesses == 0);
    CloseHandle(job);

    SIZE_T size = 0;
    SetLastError(0);
    check("attribute list size query", !InitializeProcThreadAttributeList(NULL, 1, 0, &size) && GetLastError() == ERROR_INSUFFICIENT_BUFFER && size);
    void *list = HeapAlloc(GetProcessHeap(), 0, size);
    check("attribute list", InitializeProcThreadAttributeList(list, 1, 0, &size));
    DeleteProcThreadAttributeList(list);
    HeapFree(GetProcessHeap(), 0, list);

    HANDLE self = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ | PROCESS_VM_WRITE | PROCESS_VM_OPERATION, FALSE, GetCurrentProcessId());
    check("OpenProcess of this process", self != NULL);
    WCHAR image[MAX_PATH];
    DWORD isz = MAX_PATH;
    check("QueryFullProcessImageNameW", QueryFullProcessImageNameW(self, 0, image, &isz) && wide_ends(image, L"platform.exe"));
    static char src[16] = "remote memory", dst[16];
    SIZE_T done = 0;
    check("ReadProcessMemory of this process", ReadProcessMemory(self, src, dst, 14, &done) && done == 14 && mem_eq(src, dst, 14));
    char *page = VirtualAllocEx(self, NULL, 4096, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
    check("VirtualAllocEx", page != NULL);
    check("WriteProcessMemory", page && WriteProcessMemory(self, page, "xyz", 3, &done) && done == 3 && page[1] == 'y');
    DWORD old = 0;
    check("VirtualProtectEx", page && VirtualProtectEx(self, page, 4096, PAGE_READONLY, &old) && old == PAGE_READWRITE);
    check("VirtualFreeEx", page && VirtualFreeEx(self, page, 0, MEM_RELEASE));
    BOOL wow = TRUE;
    check("IsWow64Process: native x64", IsWow64Process(self, &wow) && !wow);
    CloseHandle(self);

    WCHAR dir[MAX_PATH];
    check("GetSystemDirectoryW", GetSystemDirectoryW(dir, MAX_PATH) > 3 && dir[1] == L':');
    check("GetWindowsDirectoryW", GetWindowsDirectoryW(dir, MAX_PATH) > 3 && dir[1] == L':');
    SetEnvironmentVariableW(L"OSYX_PLATFORM_X", L"value");
    check("ExpandEnvironmentStringsW", ExpandEnvironmentStringsW(L"<%OSYX_PLATFORM_X%>", dir, MAX_PATH) == 8 && wide_eq(dir, L"<value>"));
    SYSTEM_INFO sys;
    GetSystemInfo(&sys);
    check("GetCurrentProcessorNumber", GetCurrentProcessorNumber() < sys.dwNumberOfProcessors);
}

static void paths(void) {
    WCHAR p[MAX_PATH] = L"C:\\dir\\file.txt";
    check("PathRemoveFileSpecW", PathRemoveFileSpecW(p) && wide_eq(p, L"C:\\dir"));
    check("PathRemoveFileSpecW to the root", PathRemoveFileSpecW(p) && wide_eq(p, L"C:\\"));
    check("PathRemoveFileSpecW at the root", !PathRemoveFileSpecW(p) && wide_eq(p, L"C:\\"));
    WCHAR q[MAX_PATH] = L"C:\\dir";
    check("PathAppendW canonicalizes", PathAppendW(q, L"sub\\..\\x.txt") && wide_eq(q, L"C:\\dir\\x.txt"));
    check("PathMatchSpecW ignores case", PathMatchSpecW(L"C:\\a\\FILE.TXT", L"*.txt"));
    check("PathMatchSpecW spec list", PathMatchSpecW(L"main.c", L"*.h; *.c"));
    check("PathMatchSpecW mismatch", !PathMatchSpecW(L"main.c", L"*.h"));
}

static void winrt(void) {
    HSTRING s = NULL;
    check("WindowsCreateString", WindowsCreateString(L"hello", 5, &s) == S_OK && s);
    UINT32 len = 0;
    const WCHAR *raw = WindowsGetStringRawBuffer(s, &len);
    check("string length and buffer", WindowsGetStringLen(s) == 5 && len == 5 && wide_eq(raw, L"hello"));
    check("WindowsDeleteString", WindowsDeleteString(s) == S_OK);
    HSTRING_HEADER header;
    HSTRING ref = NULL;
    check("WindowsCreateStringReference", WindowsCreateStringReference(L"abc", 3, &header, &ref) == S_OK && WindowsGetStringLen(ref) == 3);
    check("empty string is NULL", WindowsCreateString(L"", 0, &s) == S_OK && s == NULL && WindowsGetStringLen(NULL) == 0);
    HSTRING cls = NULL;
    void *factory = (void *)1;
    WindowsCreateStringReference(L"OSyx.NoSuchClass", 16, &header, &cls);
    static const GUID iid = {0x00000035, 0, 0, {0xc0, 0, 0, 0, 0, 0, 0, 0x46}}; /* IActivationFactory */
    check("an unknown runtime class fails", FAILED(RoGetActivationFactory(cls, &iid, &factory)) && factory == NULL);
}

void __stdcall start(void) {
    files();
    nls();
    int en_us = 0;
    check("EnumSystemLocalesEx lists en-US", EnumSystemLocalesEx(find_en_us, LOCALE_ALL, (LPARAM)&en_us, NULL) && en_us);
    pool();
    processes();
    paths();
    winrt();
    finish("platform");
}
