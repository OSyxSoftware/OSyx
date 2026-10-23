/* The kernel32 imports of nss3.dll (NSPR and SQLite): ANSI file and
 * process calls, the ANSI environment block, OVERLAPPED completion,
 * byte-range locks, anonymous pipes, memory status, heap checks, named
 * semaphore lookup, handle information and thread priorities. */
#include "common.h"

static DWORD len_of(const char *s) {
    DWORD n = 0;
    while (s[n]) n++;
    return n;
}

static int str_eq(const char *a, const char *b) {
    while (*a && *a == *b) a++, b++;
    return *a == *b;
}

static void append(char *dst, const char *src) {
    dst += len_of(dst);
    while ((*dst++ = *src++)) {}
}

/* Whether the ANSI environment block holds the entry `want`. */
static int block_has(const char *b, const char *want) {
    for (; *b; b += len_of(b) + 1)
        if (str_eq(b, want)) return 1;
    return 0;
}

void __stdcall start(void) {
    /* Environment */
    SetEnvironmentVariableA("OSYX_K32", "ansi");
    char *env = GetEnvironmentStrings();
    check("GetEnvironmentStrings holds OSYX_K32=ansi", env != NULL && block_has(env, "OSYX_K32=ansi"));
    check("FreeEnvironmentStringsA", FreeEnvironmentStringsA(env));

    /* A file written through an OVERLAPPED structure. */
    char path[MAX_PATH], pattern[MAX_PATH];
    GetTempPathA(MAX_PATH, path);
    append(path, "k32ansi.tmp");
    HANDLE h = CreateFileA(path, GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, CREATE_ALWAYS, 0, NULL);
    check("CreateFileA", h != INVALID_HANDLE_VALUE);
    HANDLE ev = CreateEventA(NULL, TRUE, FALSE, NULL);
    OVERLAPPED ov = {0};
    ov.hEvent = ev;
    DWORD n = 0;
    check("WriteFile with an OVERLAPPED completes", WriteFile(h, "hello", 5, &n, &ov) && n == 5);
    n = 0;
    check("GetOverlappedResult reports 5 bytes", GetOverlappedResult(h, &ov, &n, FALSE) && n == 5);
    check("... and the event is signaled", WaitForSingleObject(ev, 0) == WAIT_OBJECT_0);
    check("CancelIo with nothing pending", CancelIo(h));
    DWORD hflags = 99;
    check("GetHandleInformation: not inheritable", GetHandleInformation(h, &hflags) && hflags == 0);

    WIN32_FILE_ATTRIBUTE_DATA fa;
    check("GetFileAttributesExA size", GetFileAttributesExA(path, GetFileExInfoStandard, &fa) && fa.nFileSizeLow == 5);
    WIN32_FIND_DATAA fd;
    GetTempPathA(MAX_PATH, pattern);
    append(pattern, "k32ansi.t?p");
    HANDLE find = FindFirstFileA(pattern, &fd);
    check("FindFirstFileA", find != INVALID_HANDLE_VALUE && str_eq(fd.cFileName, "k32ansi.tmp") && fd.nFileSizeLow == 5);
    check("FindNextFileA: no more files", !FindNextFileA(find, &fd) && GetLastError() == ERROR_NO_MORE_FILES);
    FindClose(find);

    /* Byte-range locks: a second handle of the same file. */
    HANDLE h2 = CreateFileA(path, GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, OPEN_EXISTING, 0, NULL);
    check("LockFile 0..10", LockFile(h, 0, 0, 10, 0));
    check("an overlapping LockFile from another handle fails",
          !LockFile(h2, 5, 0, 10, 0) && GetLastError() == ERROR_LOCK_VIOLATION);
    check("... and from the same handle", !LockFile(h, 0, 0, 1, 0) && GetLastError() == ERROR_LOCK_VIOLATION);
    check("a disjoint LockFile succeeds", LockFile(h2, 10, 0, 10, 0));
    OVERLAPPED at = {0};
    at.Offset = 30;
    check("shared LockFileEx 30..40", LockFileEx(h, 0, 0, 10, 0, &at));
    at.Offset = 35;
    check("an overlapping shared lock succeeds", LockFileEx(h2, LOCKFILE_FAIL_IMMEDIATELY, 0, 10, 0, &at));
    check("an overlapping exclusive lock fails",
          !LockFileEx(h2, LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY, 0, 1, 0, &at) && GetLastError() == ERROR_LOCK_VIOLATION);
    check("UnlockFile of a range never locked fails", !UnlockFile(h, 0, 0, 5, 0) && GetLastError() == ERROR_NOT_LOCKED);
    check("UnlockFile 0..10", UnlockFile(h, 0, 0, 10, 0));
    check("the range is free again", LockFile(h2, 0, 0, 10, 0) && UnlockFile(h2, 0, 0, 10, 0));
    check("UnlockFileEx 35..45", UnlockFileEx(h2, 0, 10, 0, &at));
    at.Offset = 30;
    check("UnlockFileEx 30..40", UnlockFileEx(h, 0, 10, 0, &at));
    CloseHandle(h2); /* releases its lock on 10..20 */
    check("closing a handle releases its locks", LockFile(h, 10, 0, 10, 0) && UnlockFile(h, 10, 0, 10, 0));
    CloseHandle(h);
    CloseHandle(ev);
    DeleteFileA(path);

    DWORD spc = 0, bps = 0, fc = 0, tc = 0;
    check("GetDiskFreeSpaceA(NULL)", GetDiskFreeSpaceA(NULL, &spc, &bps, &fc, &tc) && bps > 0 && tc > 0);

    /* Anonymous pipe */
    HANDLE pr, pw;
    check("CreatePipe", CreatePipe(&pr, &pw, NULL, 0));
    check("GetFileType is FILE_TYPE_PIPE", GetFileType(pr) == FILE_TYPE_PIPE);
    char buf[8];
    check("data crosses the pipe", WriteFile(pw, "pipe", 4, &n, NULL) && n == 4 && ReadFile(pr, buf, 4, &n, NULL) && n == 4 &&
                                      mem_eq(buf, "pipe", 4));
    CloseHandle(pw);
    check("reading a pipe without writers ends", !ReadFile(pr, buf, 4, &n, NULL) || n == 0);
    CloseHandle(pr);

    /* Memory and heap */
    MEMORYSTATUSEX ms = {0};
    check("GlobalMemoryStatusEx needs dwLength", !GlobalMemoryStatusEx(&ms));
    ms.dwLength = sizeof ms;
    check("GlobalMemoryStatusEx", GlobalMemoryStatusEx(&ms) && ms.ullTotalPhys > 0 && ms.ullAvailPhys <= ms.ullTotalPhys &&
                                      ms.dwMemoryLoad <= 100 && ms.ullTotalVirtual > 0);
    check("HeapValidate of the process heap", HeapValidate(GetProcessHeap(), 0, NULL));
    SetLastError(1234);
    check("HeapCompact reports no error", HeapCompact(GetProcessHeap(), 0) != 0 || GetLastError() == 0);

    check("OpenSemaphoreA of an unknown name", OpenSemaphoreA(SEMAPHORE_ALL_ACCESS, FALSE, "osyx-no-such-semaphore") == NULL &&
                                                   GetLastError() == ERROR_FILE_NOT_FOUND);

    check("SetThreadPriority(BELOW_NORMAL)", SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL));
    check_eq("GetThreadPriority", GetThreadPriority(GetCurrentThread()), THREAD_PRIORITY_BELOW_NORMAL);
    check("an invalid priority fails", !SetThreadPriority(GetCurrentThread(), 7) && GetLastError() == ERROR_INVALID_PARAMETER);
    SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_NORMAL);

    /* CreateProcessA: minimal.exe (exit code 42) next to this program. */
    char exe[MAX_PATH], line[MAX_PATH + 4] = "\"";
    DWORD k = GetModuleFileNameA(NULL, exe, MAX_PATH);
    while (k > 0 && exe[k - 1] != '\\' && exe[k - 1] != '/') k--;
    exe[k] = 0;
    append(exe, "minimal.exe");
    append(line, exe);
    append(line, "\"");
    STARTUPINFOA si = {0};
    si.cb = sizeof si;
    PROCESS_INFORMATION pi;
    check("CreateProcessA(minimal.exe)", CreateProcessA(NULL, line, NULL, NULL, FALSE, 0, NULL, NULL, &si, &pi));
    WaitForSingleObject(pi.hProcess, INFINITE);
    DWORD code = 99;
    check("... exits with 42", GetExitCodeProcess(pi.hProcess, &code) && code == 42);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    finish("k32ansi");
}
