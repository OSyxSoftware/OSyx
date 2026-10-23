/* PE32+ DLL fixture for the OSyx PE parser tests: named and ordinal-only
 * exports, an export forwarder (see sample_dll.def), a TLS directory with one
 * callback, absolute pointers that need DIR64 base relocations, and functions
 * with unwind data (.pdata). No C runtime. Never executed by the tests. */
#include <windows.h>

/* --- TLS: what a C runtime would normally provide ---------------------- */
ULONG _tls_index = 0;
__attribute__((section(".tls"))) char _tls_start = 0;
__attribute__((section(".tls$ZZZ"))) char _tls_end = 0;

static void NTAPI tls_callback(PVOID module, DWORD reason, PVOID reserved) {
    (void)module;
    (void)reason;
    (void)reserved;
}

__attribute__((section(".CRT$XLB"), used)) PIMAGE_TLS_CALLBACK tls_callback_entry = tls_callback;
__attribute__((section(".CRT$XLZ"), used)) PIMAGE_TLS_CALLBACK tls_callback_end = 0;

__attribute__((used)) const IMAGE_TLS_DIRECTORY64 _tls_used = {
    (ULONGLONG)&_tls_start,
    (ULONGLONG)&_tls_end,
    (ULONGLONG)&_tls_index,
    (ULONGLONG)&tls_callback_entry,
    0,
    0,
};

/* --- exports ------------------------------------------------------------ */
static int counter = 0;
int *counter_pointer = &counter; /* absolute address: needs a relocation */

__declspec(dllexport) int add_numbers(int a, int b) { return a + b; }

__declspec(dllexport) int bump_counter(void) { return ++*counter_pointer; }

int ordinal_only(int x) { return x * 3; } /* exported by ordinal in the .def */

/* Has a stack frame, so the compiler emits unwind data (.pdata/.xdata). */
__declspec(dllexport) int checksum(const char *text) {
    volatile char copy[64];
    int sum = 0;
    for (int i = 0; i < 63 && text[i]; i++) {
        copy[i] = text[i];
        sum += copy[i] * add_numbers(i, 1);
    }
    return sum;
}

BOOL WINAPI DllMainCRTStartup(HINSTANCE instance, DWORD reason, LPVOID reserved) {
    (void)instance;
    (void)reason;
    (void)reserved;
    return TRUE;
}
