/* Target B: a basic kernel32 console program: GetStdHandle, WriteFile,
 * GetLastError, ExitProcess, plus the process environment (command line,
 * PEB/TEB through gs:, module handle). */
#include "common.h"

void __stdcall start(void) {
    put("Hello from a Windows PE32+ program on OSyx\r\n");
    HANDLE out = GetStdHandle(STD_OUTPUT_HANDLE);
    check("GetStdHandle(STD_OUTPUT_HANDLE) is valid", out != NULL && out != INVALID_HANDLE_VALUE);
    DWORD w = 0;
    check("WriteFile reports the bytes written", WriteFile(out, "x\r\n", 3, &w, NULL) && w == 3);

    SetLastError(0);
    check("CloseHandle on a bad handle fails", !CloseHandle((HANDLE)(ULONG_PTR)0x7777));
    check_eq("GetLastError is ERROR_INVALID_HANDLE", GetLastError(), ERROR_INVALID_HANDLE);
    SetLastError(1234);
    check_eq("SetLastError/GetLastError round trip", GetLastError(), 1234);

    /* The TEB is reachable through gs:[0x30] and points to the PEB
     * (documented x64 offsets: TEB 0x08 StackBase, 0x10 StackLimit,
     * 0x30 Self, 0x40 ClientId.UniqueProcess, 0x60 PEB, 0x68 LastError;
     * PEB 0x10 ImageBaseAddress). */
    char *teb = (char *)NtCurrentTeb();
    check("TEB self pointer (gs:[0x30])", teb != NULL && *(char **)(teb + 0x30) == teb);
    char *peb = *(char **)(teb + 0x60);
    check("PEB image base is this module", peb != NULL && *(HMODULE *)(peb + 0x10) == GetModuleHandleA(NULL));
    check_eq("last error lives in the TEB", *(DWORD *)(teb + 0x68), 1234);
    char *base = *(char **)(teb + 0x08), *limit = *(char **)(teb + 0x10);
    check("stack limits in the TEB", base > limit && (char *)&w < base && (char *)&w >= limit);
    const char *cmd = GetCommandLineA();
    check("command line is available", cmd != NULL && str_len(cmd) > 0);
    put("command line: ");
    put(cmd);
    put("\r\n");
    check_eq("GetCurrentProcessId matches the TEB", GetCurrentProcessId(), (DWORD)*(ULONG_PTR *)(teb + 0x40));
    char path[MAX_PATH];
    DWORD n = GetModuleFileNameA(NULL, path, MAX_PATH);
    check("GetModuleFileNameA names hello.exe", n > 9 && mem_eq(path + n - 9, "hello.exe", 9));
    check("GetModuleHandleA(\"kernel32.dll\")", GetModuleHandleA("KERNEL32.DLL") != NULL);
    check("GetProcAddress finds WriteFile", GetProcAddress(GetModuleHandleA("kernel32.dll"), "WriteFile") == (FARPROC)WriteFile);
    SetLastError(0);
    check("GetProcAddress of a missing export fails", GetProcAddress(GetModuleHandleA("kernel32.dll"), "NoSuchFunction") == NULL);
    check_eq("... with ERROR_PROC_NOT_FOUND", GetLastError(), ERROR_PROC_NOT_FOUND);
    HMODULE from = NULL;
    const DWORD by_address = GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
    check("GetModuleHandleExW finds the module of an address", GetModuleHandleExW(by_address, (LPCWSTR)start, &from) && from == GetModuleHandleA(NULL));
    check("an address in no module is not found", !GetModuleHandleExW(by_address, (LPCWSTR)&from, &from) && from == NULL && GetLastError() == ERROR_MOD_NOT_FOUND);
    check("PIN contradicts UNCHANGED_REFCOUNT",
          !GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_PIN | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, NULL, &from) && GetLastError() == ERROR_INVALID_PARAMETER);
    char env[64];
    check("GetEnvironmentVariableA(\"OSYX\")", GetEnvironmentVariableA("OSYX", env, sizeof env) == 1 && env[0] == '1');
    finish("hello");
}
