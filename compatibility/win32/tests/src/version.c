#include "common.h"
#include <winver.h>
void __stdcall start(void) {
    WCHAR name[512]; DWORD unused = 123;
    GetModuleFileNameW(NULL, name, 512);
    DWORD size = GetFileVersionInfoSizeW(name, &unused);
    check("version resource size and deprecated handle", size > 52 && unused == 0);
    BYTE *data = HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, size);
    check("version read rejects short buffer", !GetFileVersionInfoW(name, 0, 1, data) && GetLastError() == ERROR_INSUFFICIENT_BUFFER);
    check("version read copies actual PE resource", GetFileVersionInfoW(name, 0, size, data));
    VS_FIXEDFILEINFO *fixed = NULL; UINT n = 0;
    check("fixed version binary query", VerQueryValueW(data, L"\\", (void **)&fixed, &n) && n == sizeof *fixed && fixed->dwSignature == 0xfeef04bd);
    check("file version matches fixture", fixed && fixed->dwFileVersionMS == 0x00010002 && fixed->dwFileVersionLS == 0x00030004);
    check("product version matches fixture", fixed && fixed->dwProductVersionMS == 0x00050006 && fixed->dwProductVersionLS == 0x00070008);
    WCHAR *text = NULL;
    check("UTF16 string version query", VerQueryValueW(data, L"\\StringFileInfo\\040904b0\\CompanyName", (void **)&text, &n) && n > 4 && text[0] == 'O' && text[1] == 'S');
    WORD *translation = NULL;
    check("translation binary query", VerQueryValueW(data, L"\\VarFileInfo\\Translation", (void **)&translation, &n) && n == 4 && translation[0] == 0x409 && translation[1] == 1200);
    check("unknown version key fails", !VerQueryValueW(data, L"\\NoSuchKey", (void **)&text, &n));
    check("missing version file fails", !GetFileVersionInfoSizeW(L"C:\\no-such-version.exe", NULL) && GetLastError() == ERROR_FILE_NOT_FOUND);
    HeapFree(GetProcessHeap(), 0, data);
    finish("version");
}
