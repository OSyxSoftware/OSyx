/* Target: a 32-bit (PE32/i386) console program that calls built-in
 * functions through the WoW64 bridge signatures: strings, MulDiv, the tick
 * count, the command line and global memory (both must be addressable by
 * 32-bit code), initialization files, and the registry, whose HKEY outputs
 * are 64-bit values on the host side (OutWide). No C runtime.
 *
 * Prints "api32: ok NAME" / "api32: FAIL NAME" lines and a summary, and
 * exits with the number of failures. Runs only under /bin/winrun inside
 * QEMU (scripts/win-test.ps1). */
typedef unsigned long DWORD;
typedef int BOOL;
typedef void *HANDLE;
typedef void *HKEY;
typedef unsigned short WCHAR;

#define STD_OUTPUT_HANDLE ((DWORD)-11)
#define HKEY_CURRENT_USER ((HKEY)(unsigned long)0x80000001)
#define KEY_ALL_ACCESS 0xF003F
#define REG_DWORD 4
#define GPTR 0x40

__declspec(dllimport) HANDLE __stdcall GetStdHandle(DWORD std_handle);
__declspec(dllimport) BOOL __stdcall WriteFile(HANDLE file, const void *buf, DWORD len, DWORD *written, void *overlapped);
__declspec(dllimport) void __stdcall ExitProcess(DWORD code);
__declspec(dllimport) int __stdcall lstrcmpiA(const char *a, const char *b);
__declspec(dllimport) int __stdcall lstrlenW(const WCHAR *s);
__declspec(dllimport) int __stdcall MulDiv(int a, int b, int c);
__declspec(dllimport) DWORD __stdcall GetTickCount(void);
__declspec(dllimport) WCHAR *__stdcall GetCommandLineW(void);
__declspec(dllimport) void *__stdcall GlobalAlloc(DWORD flags, DWORD size);
__declspec(dllimport) void *__stdcall GlobalFree(void *mem);
__declspec(dllimport) DWORD __stdcall GetPrivateProfileStringW(const WCHAR *section, const WCHAR *key, const WCHAR *def,
                                                               WCHAR *buf, DWORD size, const WCHAR *file);
__declspec(dllimport) BOOL __stdcall WritePrivateProfileStringW(const WCHAR *section, const WCHAR *key, const WCHAR *value,
                                                                const WCHAR *file);
__declspec(dllimport) BOOL __stdcall DeleteFileW(const WCHAR *path);
__declspec(dllimport) long __stdcall RegCreateKeyExW(HKEY key, const WCHAR *sub, DWORD reserved, WCHAR *cls, DWORD options,
                                                     DWORD sam, void *sa, HKEY *result, DWORD *disposition);
__declspec(dllimport) long __stdcall RegSetValueExW(HKEY key, const WCHAR *name, DWORD reserved, DWORD type,
                                                    const void *data, DWORD size);
__declspec(dllimport) long __stdcall RegQueryValueExW(HKEY key, const WCHAR *name, DWORD *reserved, DWORD *type,
                                                      void *data, DWORD *size);
__declspec(dllimport) long __stdcall RegCloseKey(HKEY key);
__declspec(dllimport) long __stdcall RegDeleteKeyW(HKEY key, const WCHAR *sub);

static HANDLE out;
static int failed;

static int len(const char *s) {
    int n = 0;
    while (s[n]) n++;
    return n;
}

static void say(const char *a, const char *b) {
    DWORD w;
    WriteFile(out, a, len(a), &w, 0);
    WriteFile(out, b, len(b), &w, 0);
    WriteFile(out, "\n", 1, &w, 0);
}

static void check(const char *name, int ok) {
    say(ok ? "api32: ok " : "api32: FAIL ", name);
    if (!ok) failed++;
}

/* "name 0x1234abcd" into buf. */
static const char *with_hex(char *buf, const char *name, unsigned long v) {
    int n = 0;
    while (*name) buf[n++] = *name++;
    buf[n++] = ' ', buf[n++] = '0', buf[n++] = 'x';
    for (int i = 7; i >= 0; i--) buf[n++] = "0123456789abcdef"[(v >> (i * 4)) & 15];
    buf[n] = 0;
    return buf;
}

static int wide_eq(const WCHAR *a, const char *b) {
    while (*b && *a == (WCHAR)*b) a++, b++;
    return *a == 0 && *b == 0;
}

void __stdcall start(void) {
    out = GetStdHandle(STD_OUTPUT_HANDLE);

    check("lstrcmpiA ignores case", lstrcmpiA("Steam", "sTEAM") == 0 && lstrcmpiA("a", "b") < 0);
    check("MulDiv rounds and rejects /0", MulDiv(10, 3, 4) == 8 && MulDiv(-10, 3, 4) == -8 && MulDiv(1, 1, 0) == -1);
    check("GetTickCount advances", GetTickCount() != 0);

    WCHAR *cmd = GetCommandLineW();
    check("GetCommandLineW is a readable 32-bit pointer", cmd != 0 && lstrlenW(cmd) > 0);

    char *mem = GlobalAlloc(GPTR, 4096);
    int zero = mem != 0;
    for (int i = 0; zero && i < 4096; i++) zero = mem[i] == 0;
    if (mem) mem[4095] = 1;
    check("GlobalAlloc(GPTR) returns zeroed 32-bit memory", zero && mem[4095] == 1);
    check("GlobalFree", mem && GlobalFree(mem) == 0);

    static const WCHAR ini[] = L"C:\\tmp\\api32.ini";
    WCHAR buf[64];
    DeleteFileW(ini);
    check("WritePrivateProfileStringW", WritePrivateProfileStringW(L"Setup", L"Path", L"C:\\Steam", ini));
    DWORD n = GetPrivateProfileStringW(L"setup", L"PATH", L"none", buf, 64, ini);
    check("GetPrivateProfileStringW reads it back", n == 8 && wide_eq(buf, "C:\\Steam"));
    n = GetPrivateProfileStringW(L"Setup", L"Missing", L"none", buf, 64, ini);
    check("GetPrivateProfileStringW default", n == 4 && wide_eq(buf, "none"));
    DeleteFileW(ini);

    HKEY key = (HKEY)0x12345678;
    DWORD disp = 0;
    long e = RegCreateKeyExW(HKEY_CURRENT_USER, L"Software\\OSyx\\api32", 0, 0, 0, KEY_ALL_ACCESS, 0, &key, &disp);
    char line[96];
    check(with_hex(line, "RegCreateKeyExW writes a 32-bit HKEY, error", (unsigned long)e),
          e == 0 && key != (HKEY)0x12345678 && key != 0);
    DWORD value = 0x5a5a1234, type = 0, got = 0, size = sizeof got;
    check("RegSetValueExW", e == 0 && RegSetValueExW(key, L"value", 0, REG_DWORD, &value, sizeof value) == 0);
    check("RegQueryValueExW", e == 0 && RegQueryValueExW(key, L"value", 0, &type, &got, &size) == 0 && type == REG_DWORD &&
                                  got == value && size == 4);
    check("RegCloseKey", e == 0 && RegCloseKey(key) == 0);
    RegDeleteKeyW(HKEY_CURRENT_USER, L"Software\\OSyx\\api32");

    say(failed ? "api32: summary FAIL" : "api32: summary ok", "");
    ExitProcess(failed);
}
