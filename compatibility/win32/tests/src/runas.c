/* ShellExecuteExW with the "runas" verb: the program starts itself with
   administrator rights after the user's consent. "runas.exe yes" expects
   the prompt to be accepted (the elevated child reports IsUserAnAdmin
   through its exit code), "runas.exe no" expects it declined
   (ERROR_CANCELLED, SE_ERR_ACCESSDENIED). The child runs with "--child".
   Needs the desktop's elevation service, so it runs in a desktop session
   (scripts/elevation-test.ps1), not in the console test run. */
#include "common.h"
#include <shellapi.h>
#include <shlobj.h>

/* Exit codes of the child. */
#define CHILD_ADMIN 81
#define CHILD_USER 80

static int has(const WCHAR *s, const WCHAR *word) {
    for (; *s; s++) {
        const WCHAR *a = s, *b = word;
        while (*b && *a == *b) { a++; b++; }
        if (!*b) return 1;
    }
    return 0;
}

void __stdcall start(void) {
    const WCHAR *line = GetCommandLineW();
    if (has(line, L"--child")) ExitProcess(IsUserAnAdmin() ? CHILD_ADMIN : CHILD_USER);
    int accept = has(line, L" yes");
    WCHAR self[MAX_PATH];
    GetModuleFileNameW(NULL, self, MAX_PATH);
    check("the caller is not an administrator", !IsUserAnAdmin());

    SHELLEXECUTEINFOW info;
    memset(&info, 0, sizeof info);
    info.cbSize = sizeof info;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = L"runas";
    info.lpFile = self;
    info.lpParameters = L"--child";
    info.nShow = SW_SHOWNORMAL;
    SetLastError(0);
    BOOL ok = ShellExecuteExW(&info);
    DWORD error = GetLastError();
    if (accept) {
        check("ShellExecuteExW(runas) succeeds once the user agrees", ok);
        check("... and returns the process", info.hProcess != NULL);
        check_eq("... hInstApp > 32", (UINT_PTR)info.hInstApp > 32, 1);
        DWORD code = 0;
        if (info.hProcess) {
            check("the elevated process ends", WaitForSingleObject(info.hProcess, 60000) == WAIT_OBJECT_0);
            GetExitCodeProcess(info.hProcess, &code);
            CloseHandle(info.hProcess);
        }
        check_eq("the elevated process is an administrator", code, CHILD_ADMIN);
    } else {
        check("ShellExecuteExW(runas) fails when the user declines", !ok);
        check_eq("... with ERROR_CANCELLED", error, ERROR_CANCELLED);
        check_eq("... and SE_ERR_ACCESSDENIED in hInstApp", (UINT_PTR)info.hInstApp, SE_ERR_ACCESSDENIED);
        check("... and no process", info.hProcess == NULL);
    }
    finish(accept ? "runas yes" : "runas no");
}
