/* Shared executable images: children started from this program map the
   parent's laid-out image (on OSyx: copy-on-write image sections). Writes
   in any process stay private, initial values come from the file, and
   relocated pointers and code work everywhere. Two children run at once so
   both map the image while the parent still holds it. Also runs on Windows.
   The program runs itself as the child with "--child <n>". */
#include "common.h"

static volatile int counter = 7;
/* Initialised data spanning many pages; the marked pages are written. */
static volatile unsigned char table[16 * 4096] = { 1 };
static const char text[] = "image text";
static int twice(int v) { return v * 2; }
/* An absolute address in the image: relocated when it is not at its base. */
static int (*volatile fn)(int) = twice;

static int child(int n) {
    /* The file's values, not the parent's changes. */
    if (counter != 7) return 2;
    if (table[0] != 1 || table[5 * 4096] != 0 || table[9 * 4096] != 0) return 3;
    if (fn(21) != 42 || !mem_eq(text, "image text", 11)) return 4;
    counter = 100 + n;
    table[5 * 4096] = (unsigned char)(50 + n);
    /* Give the sibling time to map the image while this one has written. */
    Sleep(200);
    if (counter != 100 + n || table[5 * 4096] != 50 + n || table[9 * 4096] != 0) return 5;
    return 0;
}

static HANDLE spawn(const WCHAR *exe, WCHAR n) {
    WCHAR line[MAX_PATH + 16];
    int k = 0;
    for (const WCHAR *s = L"imageshare.exe --child "; *s; s++) line[k++] = *s;
    line[k++] = n;
    line[k] = 0;
    STARTUPINFOW si = { sizeof si };
    PROCESS_INFORMATION pi;
    if (!CreateProcessW(exe, line, NULL, NULL, FALSE, 0, NULL, NULL, &si, &pi)) return NULL;
    CloseHandle(pi.hThread);
    return pi.hProcess;
}

void start(void) {
    const WCHAR *at = GetCommandLineW();
    for (; *at; at++)
        if (at[0] == L'-' && at[1] == L'-' && at[2] == L'c' && at[3] == L'h' && at[4] == L'i' && at[5] == L'l' && at[6] == L'd')
            ExitProcess(child(at[8] - L'0'));
    counter = 8;
    table[5 * 4096] = 9;
    check("code and read-only data in the image", fn(4) == 8 && mem_eq(text, "image text", 11));
    WCHAR exe[MAX_PATH];
    GetModuleFileNameW(NULL, exe, MAX_PATH);
    HANDLE a = spawn(exe, L'1'), b = spawn(exe, L'2');
    check("two children of the same program start", a && b);
    DWORD ca = 1, cb = 1;
    if (a && b) {
        HANDLE both[2] = { a, b };
        check("both children finish", WaitForMultipleObjects(2, both, TRUE, 20000) == WAIT_OBJECT_0);
        GetExitCodeProcess(a, &ca);
        GetExitCodeProcess(b, &cb);
    }
    check_eq("child 1 sees the file's data and keeps its writes private", ca, 0);
    check_eq("child 2 sees the file's data and keeps its writes private", cb, 0);
    check("the parent's writes survive the children", counter == 8 && table[5 * 4096] == 9 && table[9 * 4096] == 0);
    finish("imageshare");
}
