/* Minimal PE32+ console program for the OSyx PE parser tests (and, later,
 * the PE loader): no C runtime, imports only GetStdHandle, WriteFile and
 * ExitProcess from kernel32. Built by scripts/gen-pe-fixtures.ps1; never
 * executed by the tests. */
#include <windows.h>

static const char message[] = "Hello from a PE32+ fixture\r\n";

void __stdcall start(void) {
    HANDLE out = GetStdHandle(STD_OUTPUT_HANDLE);
    DWORD written = 0;
    WriteFile(out, message, sizeof(message) - 1, &written, NULL);
    ExitProcess(written == sizeof(message) - 1 ? 0 : 1);
}
