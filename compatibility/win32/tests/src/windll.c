/* A DLL loaded at run time by threads.exe: exports by name and ordinal, a
 * forwarder to kernel32 (windll.def), an import through the API set
 * contract api-ms-win-core-processthreads-l1-1-0 (resolved by the OSyx
 * schema), DllMain notifications and a base relocation. No C runtime. */
#include <windows.h>

static volatile LONG attaches;
static LONG *volatile counter = (LONG *)&attaches; /* absolute: relocated */

__declspec(dllexport) int __cdecl multiply(int a, int b) { return a * b; }

/* Exported by ordinal 5 (windll.def). */
DWORD __cdecl attach_count(void) { return (DWORD)*counter; }

/* GetCurrentThreadId comes from the API set import library
 * (apiset-processthreads.def), linked before kernel32. */

BOOL WINAPI DllMainCRTStartup(HINSTANCE instance, DWORD reason, LPVOID reserved) {
    (void)instance;
    (void)reserved;
    if (reason == DLL_PROCESS_ATTACH) {
        InterlockedIncrement(&attaches);
        return GetCurrentThreadId() != 0;
    }
    return TRUE;
}
