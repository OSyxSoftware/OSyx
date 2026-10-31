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
__declspec(dllimport) DWORD __stdcall GetFileAttributesW(const WCHAR *path);
__declspec(dllimport) HANDLE __stdcall CreateFileW(const WCHAR *name, DWORD access, DWORD share, void *sa, DWORD disposition,
                                                   DWORD flags, HANDLE tmpl);
__declspec(dllimport) BOOL __stdcall ReadFile(HANDLE file, void *buf, DWORD len, DWORD *read, void *overlapped);
__declspec(dllimport) BOOL __stdcall CloseHandle(HANDLE h);
__declspec(dllimport) HANDLE __stdcall GetModuleHandleW(const WCHAR *name);
__declspec(dllimport) HANDLE __stdcall LoadLibraryExW(const WCHAR *name, HANDLE file, DWORD flags);
__declspec(dllimport) void *__stdcall GetProcAddress(HANDLE module, const char *name);
__declspec(dllimport) DWORD __stdcall GetLastError(void);
__declspec(dllimport) BOOL __stdcall FreeLibrary(HANDLE module);
__declspec(dllimport) HANDLE __stdcall LocalFree(HANDLE memory);
__declspec(dllimport) long __stdcall RegCreateKeyExW(HKEY key, const WCHAR *sub, DWORD reserved, WCHAR *cls, DWORD options,
                                                     DWORD sam, void *sa, HKEY *result, DWORD *disposition);
__declspec(dllimport) long __stdcall RegSetValueExW(HKEY key, const WCHAR *name, DWORD reserved, DWORD type,
                                                    const void *data, DWORD size);
__declspec(dllimport) long __stdcall RegQueryValueExW(HKEY key, const WCHAR *name, DWORD *reserved, DWORD *type,
                                                      void *data, DWORD *size);
__declspec(dllimport) long __stdcall RegCloseKey(HKEY key);
__declspec(dllimport) long __stdcall RegDeleteKeyW(HKEY key, const WCHAR *sub);
__declspec(dllimport) long __stdcall RegEnumKeyW(HKEY key, DWORD index, WCHAR *name, DWORD size);
__declspec(dllimport) BOOL __stdcall CreateDirectoryW(const WCHAR *name, void *attributes);
__declspec(dllimport) BOOL __stdcall RemoveDirectoryW(const WCHAR *name);
__declspec(dllimport) BOOL __stdcall SetFileSecurityW(const WCHAR *name, DWORD info, const void *descriptor);
__declspec(dllimport) DWORD __stdcall GetNamedSecurityInfoW(const WCHAR *name, DWORD kind, DWORD info,
                                                           void **owner, void **group, void **dacl, void **sacl, void **descriptor);

typedef long (__stdcall *WNDPROC)(HANDLE, DWORD, DWORD, long);
typedef struct {
    DWORD style;
    WNDPROC proc;
    int cls_extra, wnd_extra;
    HANDLE instance, icon, cursor, background;
    const WCHAR *menu, *name;
} WNDCLASS32;
_Static_assert(sizeof(WNDCLASS32) == 40, "32-bit WNDCLASSW");
__declspec(dllimport) unsigned short __stdcall RegisterClassW(const WNDCLASS32 *wc);
__declspec(dllimport) BOOL __stdcall GetClassInfoW(HANDLE instance, const WCHAR *name, WNDCLASS32 *wc);
__declspec(dllimport) long __stdcall CallWindowProcW(WNDPROC proc, HANDLE hwnd, DWORD msg, DWORD w, long l);
__declspec(dllimport) const WCHAR *__stdcall CharPrevW(const WCHAR *start, const WCHAR *current);
__declspec(dllimport) const WCHAR *__stdcall CharNextW(const WCHAR *p);
__declspec(dllimport) const char *__stdcall CharNextA(const char *p);
typedef struct { HANDLE hwnd; DWORD message, wparam; long lparam; DWORD time; long x, y; } MSG32;
_Static_assert(sizeof(MSG32) == 28, "32-bit MSG");
__declspec(dllimport) BOOL __stdcall PeekMessageW(MSG32 *msg, HANDLE hwnd, DWORD min, DWORD max, DWORD flags);
__declspec(dllimport) long __stdcall DispatchMessageW(const MSG32 *msg);
__declspec(dllimport) void __stdcall PostQuitMessage(int code);
typedef struct { HANDLE icon; int index; DWORD attributes; WCHAR display[260], kind[80]; } SHFILEINFO32;
_Static_assert(sizeof(SHFILEINFO32) == 692, "32-bit SHFILEINFOW");
__declspec(dllimport) DWORD __stdcall SHGetFileInfoW(const WCHAR *path, DWORD attrs, SHFILEINFO32 *info, DWORD size, DWORD flags);
typedef struct { HANDLE owner; DWORD func; const WCHAR *from, *to; unsigned short flags; int aborted; HANDLE mappings; const WCHAR *title; } SHFILEOP32;
_Static_assert(sizeof(SHFILEOP32) == 32, "32-bit SHFILEOPSTRUCTW");
__declspec(dllimport) int __stdcall SHFileOperationW(SHFILEOP32 *op);
typedef struct { DWORD size; HANDLE owner, instance; const WCHAR *text, *caption; DWORD style; const WCHAR *icon; DWORD context, callback, language; } MSGBOXPARAMS32;
_Static_assert(sizeof(MSGBOXPARAMS32) == 40, "32-bit MSGBOXPARAMSW");
__declspec(dllimport) int __stdcall MessageBoxIndirectW(const MSGBOXPARAMS32 *params);
__declspec(dllimport) HANDLE __stdcall LoadBitmapW(HANDLE instance, const WCHAR *name);
__declspec(dllimport) void __stdcall InitCommonControls(void);
__declspec(dllimport) HANDLE __stdcall ImageList_Create(int cx, int cy, DWORD flags, int initial, int grow);
__declspec(dllimport) int __stdcall ImageList_AddMasked(HANDLE list, HANDLE bitmap, DWORD mask);
__declspec(dllimport) BOOL __stdcall ImageList_Destroy(HANDLE list);
__declspec(dllimport) HANDLE __stdcall CreateBitmap(int width, int height, DWORD planes, DWORD bpp, const void *bits);
__declspec(dllimport) BOOL __stdcall DeleteObject(HANDLE object);
__declspec(dllimport) int __cdecl wsprintfW(WCHAR *out, const WCHAR *format, ...);
__declspec(dllimport) int __cdecl wsprintfA(char *out, const char *format, ...);
__declspec(dllimport) HANDLE __stdcall CreatePopupMenu(void);
__declspec(dllimport) BOOL __stdcall AppendMenuW(HANDLE menu, DWORD flags, DWORD id, const WCHAR *text);
__declspec(dllimport) DWORD __stdcall EnableMenuItem(HANDLE menu, DWORD id, DWORD flags);
__declspec(dllimport) BOOL __stdcall DestroyMenu(HANDLE menu);
__declspec(dllimport) BOOL __stdcall ExitWindowsEx(DWORD flags, DWORD reason);

static long __stdcall test_wndproc(HANDLE hwnd, DWORD msg, DWORD w, long l) {
    return hwnd == (HANDLE)7 && msg == 8 && w == 9 && l == -10 ? -42 : 0;
}

typedef struct {
    DWORD cb;
    WCHAR *reserved, *desktop, *title;
    DWORD x, y, xsize, ysize, xchars, ychars, fill, flags;
    unsigned short show, cb_reserved2;
    unsigned char *reserved2;
    HANDLE std_in, std_out, std_err;
} STARTUPINFOW;
typedef struct {
    HANDLE hProcess, hThread;
    DWORD dwProcessId, dwThreadId;
} PROCESS_INFORMATION;
_Static_assert(sizeof(STARTUPINFOW) == 68, "32-bit STARTUPINFOW");
_Static_assert(sizeof(PROCESS_INFORMATION) == 16, "32-bit PROCESS_INFORMATION");

__declspec(dllimport) BOOL __stdcall CreateProcessW(const WCHAR *app, WCHAR *cmdline, void *psa, void *tsa, BOOL inherit,
                                                    DWORD flags, void *env, const WCHAR *cwd, STARTUPINFOW *si,
                                                    PROCESS_INFORMATION *pi);
__declspec(dllimport) DWORD __stdcall WaitForSingleObject(HANDLE h, DWORD ms);
__declspec(dllimport) BOOL __stdcall GetExitCodeProcess(HANDLE process, DWORD *code);

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

    HANDLE kernel = GetModuleHandleW(L"kernel32.dll");
    check("GetModuleHandleW finds PE32 builtin", kernel != 0);
    HANDLE loaded_kernel = LoadLibraryExW(L"kernel32.dll", 0, 0);
    check("LoadLibraryExW returns builtin", kernel && loaded_kernel == kernel);
    typedef HANDLE (__stdcall *GetCurrentProcessFn)(void);
    GetCurrentProcessFn current = (GetCurrentProcessFn)GetProcAddress(kernel, "GetCurrentProcess");
    check("GetProcAddress creates callable PE32 gate", current && current() == (HANDLE)-1);
    check("GetProcAddress reuses PE32 gate", current && (void *)current == GetProcAddress(kernel, "GetCurrentProcess"));
    check("GetProcAddress reports missing export", GetProcAddress(kernel, "OSyxMissingExport") == 0 && GetLastError() == 127);
    check("FreeLibrary builtin", loaded_kernel && FreeLibrary(loaded_kernel));
    HANDLE plugin = LoadLibraryExW(L"plugin32.dll", 0, 0);
    check("LoadLibraryExW loads PE32 DLL", plugin != 0);
    typedef int (__cdecl *PluginValueFn)(void);
    PluginValueFn plugin_value = plugin ? (PluginValueFn)GetProcAddress(plugin, "plugin_value") : 0;
    check("PE32 export runs after DllMain and calls import", plugin_value && plugin_value() == 42);
    check("FreeLibrary PE32 DLL", plugin && FreeLibrary(plugin));

    static const WCHAR class_name[] = L"OSyxApi32Test";
    WNDCLASS32 wc = { 3, test_wndproc, 0, 0, 0, 0, 0, (HANDLE)17, 0, class_name };
    struct { WNDCLASS32 wc; DWORD guard; } info = { { 0 }, 0xfeedbeef };
    check("RegisterClassW 32-bit layout", RegisterClassW(&wc) != 0);
    check("GetClassInfoW 32-bit layout", GetClassInfoW(0, class_name, &info.wc) && info.wc.style == 3 &&
          info.wc.proc == test_wndproc && info.wc.background == (HANDLE)17 && info.guard == 0xfeedbeef);
    check("CallWindowProcW invokes 32-bit callback", CallWindowProcW(test_wndproc, (HANDLE)7, 8, 9, -10) == -42);
    const WCHAR *wide_text = L"ab";
    const char *ansi_text = "a";
    check("CharPrevW bounds", CharPrevW(wide_text, wide_text) == wide_text && CharPrevW(wide_text, wide_text + 2) == wide_text + 1);
    check("CharNextW terminator", CharNextW(wide_text) == wide_text + 1 && CharNextW(wide_text + 2) == wide_text + 2);
    check("CharNextA terminator", CharNextA(ansi_text) == ansi_text + 1 && CharNextA(ansi_text + 1) == ansi_text + 1);
    struct { MSG32 msg; DWORD guard; } message = { { 0 }, 0x1234abcd };
    PostQuitMessage(37);
    check("PeekMessageW writes 32-bit MSG", PeekMessageW(&message.msg, 0, 0, 0, 1) && message.msg.message == 0x12 &&
          message.msg.wparam == 37 && message.guard == 0x1234abcd);
    check("DispatchMessageW reads 32-bit MSG", DispatchMessageW(&message.msg) == 0);
    struct { SHFILEINFO32 info; DWORD guard; } fileinfo;
    fileinfo.guard = 0xdeadbeef;
    check("SHGetFileInfoW 32-bit layout", SHGetFileInfoW(L"C:\\test.txt", 0x80, &fileinfo.info, sizeof fileinfo.info,
          0x10 | 0x200 | 0x400) != 0 && wide_eq(fileinfo.info.display, "test.txt") &&
          wide_eq(fileinfo.info.kind, "Text Document") && fileinfo.guard == 0xdeadbeef);
    MSGBOXPARAMS32 box = { 40, 0, 0, 0, 0, 0, 0, 0, 1, 0 };
    check("MessageBoxIndirectW rejects unsupported callback", MessageBoxIndirectW(&box) == 0);
    check("LoadBitmapW reports missing resource", LoadBitmapW(0, (const WCHAR *)1) == 0);
    InitCommonControls();
    check("InitCommonControls ordinal 17 registers class", GetClassInfoW(0, L"msctls_progress32", &info.wc) != 0);
    HANDLE list = ImageList_Create(16, 16, 0, 1, 1);
    HANDLE bitmap = CreateBitmap(32, 16, 1, 32, 0);
    check("ImageList_Create and bitmap", list && bitmap);
    check("ImageList_AddMasked adds bitmap strip", list && bitmap && ImageList_AddMasked(list, bitmap, 0) == 0 &&
          ImageList_AddMasked(list, bitmap, 0) == 2);
    check("ImageList_Destroy validates handle", list && ImageList_Destroy(list) && !ImageList_Destroy(list));
    if (bitmap) DeleteObject(bitmap);
    WCHAR formatted[64];
    char formatted_a[64];
    check("wsprintfW reads i386 cdecl and 64-bit arguments", wsprintfW(formatted, L"%s:%d:%I64u", L"abc", -7, 0x100000002ULL) == 17 &&
          wide_eq(formatted, "abc:-7:4294967298"));
    check("wsprintfA reads i386 cdecl arguments", wsprintfA(formatted_a, "%08x %s", 42, "ok") == 11 &&
          lstrcmpiA(formatted_a, "0000002a ok") == 0);
    HANDLE menu = CreatePopupMenu(), submenu = CreatePopupMenu();
    check("popup menu stores nested items", menu && submenu && AppendMenuW(submenu, 0, 42, L"Item") &&
          AppendMenuW(menu, 0x10, (DWORD)submenu, L"Submenu"));
    check("EnableMenuItem updates and returns previous state", EnableMenuItem(menu, 42, 1) == 0 &&
          EnableMenuItem(menu, 42, 0) == 1 && EnableMenuItem(menu, 99, 0) == (DWORD)-1);
    check("DestroyMenu releases submenus", DestroyMenu(menu) && !DestroyMenu(submenu));
    check("ExitWindowsEx rejects invalid flags", !ExitWindowsEx(0x80, 0) && GetLastError() == 87);
    static const WCHAR source[] = L"C:\\tmp\\sh32-source.bin\0", copied[] = L"C:\\tmp\\sh32-copy.bin\0", moved[] = L"C:\\tmp\\sh32-moved.bin\0";
    DeleteFileW(source); DeleteFileW(copied); DeleteFileW(moved);
    {
        /* Self-relative descriptor, DACL only: allow Everyone (S-1-1-0). */
        static const unsigned char selfrel[] = {
            1, 0, 0x04, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 20, 0, 0, 0,
            2, 0, 28, 0, 1, 0, 0, 0,
            0, 0, 20, 0, 0xff, 0x01, 0x1f, 0x00, 1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0 };
        struct { DWORD length; DWORD descriptor; int inherit; } attributes = { 12, (DWORD)selfrel, 0 };
        WCHAR directory[] = L"C:\\tmp\\sd32dir";
        RemoveDirectoryW(directory);
        check("CreateDirectoryW accepts a 12-byte SECURITY_ATTRIBUTES", CreateDirectoryW(directory, &attributes) &&
              GetFileAttributesW(directory) != (DWORD)-1);
        RemoveDirectoryW(directory);
        attributes.length = 24;
        check("CreateDirectoryW rejects a 64-bit SECURITY_ATTRIBUTES size", !CreateDirectoryW(directory, &attributes));
        RemoveDirectoryW(directory);
    }
    HANDLE source_file = CreateFileW(source, 0x40000000, 0, 0, 2, 0, 0);
    if (source_file && source_file != (HANDLE)-1) CloseHandle(source_file);
    SHFILEOP32 op = { 0, 2, source, copied, 0x14, 0, 0, 0 };
    check("SHFileOperationW copies PE32 path", source_file != (HANDLE)-1 && SHFileOperationW(&op) == 0 &&
          !op.aborted && GetFileAttributesW(copied) != (DWORD)-1);
    op.func = 1; op.from = copied; op.to = moved;
    check("SHFileOperationW moves PE32 path", SHFileOperationW(&op) == 0 && GetFileAttributesW(moved) != (DWORD)-1 &&
          GetFileAttributesW(copied) == (DWORD)-1);
    op.func = 3; op.from = moved; op.to = 0;
    check("SHFileOperationW deletes PE32 path", SHFileOperationW(&op) == 0 && GetFileAttributesW(moved) == (DWORD)-1);
    check("SHFileOperationW reports failure", SHFileOperationW(&op) != 0 && op.aborted);
    struct { unsigned char revision, reserved; unsigned short control; DWORD owner, group, sacl, dacl; } sec32 = { 1, 0, 4, 0, 0, 0, 0 };
    check("SetFileSecurityW validates descriptor", !SetFileSecurityW(source, 4, 0));
    check("SetFileSecurityW applies PE32 null DACL", SetFileSecurityW(source, 4, &sec32));
    unsigned char *acl = 0;
    void *security_descriptor = 0;
    DWORD status = GetNamedSecurityInfoW(source, 1, 4, 0, 0, (void **)&acl, 0, &security_descriptor);
    int everyone_write = 0;
    if (status == 0 && acl) {
        unsigned char *ace = acl + 8;
        for (int i = 0; i < 2; i++) ace += *(unsigned short *)(ace + 2);
        everyone_write = (*(DWORD *)(ace + 4) & 2) != 0;
    }
    check("SetFileSecurityW changes native permissions", status == 0 && everyone_write);
    if (security_descriptor) LocalFree(security_descriptor);
    DeleteFileW(source); DeleteFileW(copied); DeleteFileW(moved);

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
    WritePrivateProfileStringW(L"Setup", L"Quoted", L"\"C:\\Program Files\"", ini);
    n = GetPrivateProfileStringW(L"Setup", L"Quoted", L"", buf, 64, ini);
    check("GetPrivateProfileStringW strips enclosing quotes", n == 16 && wide_eq(buf, "C:\\Program Files"));
    DeleteFileW(ini);
    buf[0] = buf[1] = 'x';
    n = GetPrivateProfileStringW(0, 0, L"none", buf, 64, ini);
    check("missing file: section list is empty", n == 0 && buf[0] == 0 && buf[1] == 0);

    /* An ANSI (non-UTF-8) byte in another line survives an update. */
    static const char ansi[] = "[A]\r\nk=caf\xe9\r\n";
    DWORD w;
    HANDLE f = CreateFileW(ini, 0x40000000, 0, 0, 2 /* CREATE_ALWAYS */, 0, 0);
    WriteFile(f, ansi, sizeof ansi - 1, &w, 0);
    CloseHandle(f);
    WritePrivateProfileStringW(L"A", L"n", L"1", ini);
    char raw[64];
    DWORD got_bytes = 0;
    f = CreateFileW(ini, 0x80000000, 1, 0, 3 /* OPEN_EXISTING */, 0, 0);
    ReadFile(f, raw, sizeof raw, &got_bytes, 0);
    CloseHandle(f);
    int e9 = 0, c3 = 0;
    for (DWORD i = 0; i < got_bytes; i++) e9 += (unsigned char)raw[i] == 0xe9, c3 += (unsigned char)raw[i] == 0xc3;
    check("WritePrivateProfileStringW keeps ANSI bytes", e9 == 1 && c3 == 0);
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
    HKEY child = 0;
    long child_result = RegCreateKeyExW(key, L"Child", 0, 0, 0, KEY_ALL_ACCESS, 0, &child, 0);
    check("RegEnumKeyW enumerates subkey", child_result == 0 && RegEnumKeyW(key, 0, buf, 64) == 0 && wide_eq(buf, "Child"));
    check("RegEnumKeyW rejects short buffer", RegEnumKeyW(key, 0, buf, 2) != 0);
    if (child) RegCloseKey(child);
    RegDeleteKeyW(key, L"Child");
    check("RegCloseKey", e == 0 && RegCloseKey(key) == 0);
    RegDeleteKeyW(HKEY_CURRENT_USER, L"Software\\OSyx\\api32");

    /* CreateProcessW: the 32-bit STARTUPINFOW (68 bytes) and
     * PROCESS_INFORMATION (16 bytes, 4-byte handles) are converted; the
     * child is hello32.exe from this program's directory, which exits 7. */
    {
        STARTUPINFOW si;
        PROCESS_INFORMATION pi;
        WCHAR cmdline[] = L"hello32.exe";
        for (int i = 0; i < (int)sizeof si; i++) ((char *)&si)[i] = 0;
        for (int i = 0; i < (int)sizeof pi; i++) ((char *)&pi)[i] = 0;
        si.cb = sizeof si;
        BOOL made = CreateProcessW(0, cmdline, 0, 0, 0, 0, 0, 0, &si, &pi);
        check("CreateProcessW starts a 32-bit child", made && pi.hProcess && pi.hThread && pi.dwProcessId);
        DWORD code = 0;
        if (made) {
            WaitForSingleObject(pi.hProcess, 20000);
            GetExitCodeProcess(pi.hProcess, &code);
            CloseHandle(pi.hThread);
            CloseHandle(pi.hProcess);
        }
        check(with_hex(line, "GetExitCodeProcess returns the child's exit code", code), made && code == 7);
        BOOL missing = CreateProcessW(L"no-such-program32.exe", 0, 0, 0, 0, 0, 0, 0, &si, &pi);
        check("CreateProcessW reports a missing program", !missing);
    }

    say(failed ? "api32: summary FAIL" : "api32: summary ok", "");
    ExitProcess(failed);
}
