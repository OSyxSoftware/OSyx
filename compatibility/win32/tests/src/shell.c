/* shell32 known folders and the ole32 task allocator. The profile
 * folders lie below the profile, CSIDL and FOLDERID lookups agree, a
 * missing folder is reported unless created or not verified (checked on
 * OSyx, where a fresh profile has no Videos folder). The same PE runs on
 * Windows. */
#include "common.h"
#include <shlobj.h>

static const GUID ID_Profile = {0x5E6C858F, 0x0E22, 0x4760, {0x9A, 0xFE, 0xEA, 0x33, 0x17, 0xB6, 0x71, 0x73}};
static const GUID ID_Roaming = {0x3EB685DB, 0x65F9, 0x4CF6, {0xA0, 0x3A, 0xE3, 0xEF, 0x65, 0x72, 0x9F, 0x3D}};
static const GUID ID_Local = {0xF1B32785, 0x6FBA, 0x4FCF, {0x9D, 0x55, 0x7B, 0x8E, 0x7F, 0x15, 0x70, 0x91}};
static const GUID ID_System = {0x1AC14E77, 0x02E7, 0x4E5D, {0xB7, 0x44, 0x2E, 0xB1, 0xAE, 0x51, 0x98, 0xB7}};
static const GUID ID_Videos = {0x18989B1D, 0x99B5, 0x455B, {0x84, 0x1C, 0xAB, 0x7C, 0x74, 0xE4, 0xDD, 0xFC}};
static const GUID ID_Unknown = {0x0511A5E1, 0x7E57, 0x4B1D, {0x0, 0x5, 0x1, 0xA, 0x7, 0xE, 0x5, 0x7}};

static int wide_eq(const WCHAR *a, const WCHAR *b) {
    while (*a && *a == *b) a++, b++;
    return *a == *b;
}

static DWORD wide_len(const WCHAR *s) {
    DWORD n = 0;
    while (s[n]) n++;
    return n;
}

static int wide_ends(const WCHAR *s, const WCHAR *tail) {
    DWORD n = wide_len(s), m = wide_len(tail);
    return n >= m && wide_eq(s + n - m, tail);
}

static int starts_with(const WCHAR *s, const WCHAR *prefix) {
    while (*prefix && *s == *prefix) s++, prefix++;
    return *prefix == 0 && *s == L'\\';
}

static int is_dir(const WCHAR *p) {
    DWORD a = GetFileAttributesW(p);
    return a != INVALID_FILE_ATTRIBUTES && (a & FILE_ATTRIBUTE_DIRECTORY);
}

void __stdcall start(void) {
    char marker[8];
    int native = GetEnvironmentVariableA("OSYX", marker, sizeof marker) > 0;

    WCHAR *profile = NULL, *roaming = NULL, *local = NULL, *sys = NULL;
    check("profile folder", SHGetKnownFolderPath(&ID_Profile, 0, NULL, &profile) == S_OK && profile && profile[1] == L':' && is_dir(profile));
    check("roaming AppData, created", SHGetKnownFolderPath(&ID_Roaming, KF_FLAG_CREATE, NULL, &roaming) == S_OK && roaming &&
                                          wide_ends(roaming, L"\\AppData\\Roaming") && is_dir(roaming));
    check("local AppData, created", SHGetKnownFolderPath(&ID_Local, KF_FLAG_CREATE, NULL, &local) == S_OK && local &&
                                        wide_ends(local, L"\\AppData\\Local") && is_dir(local));
    check("AppData lies in the profile", profile && roaming && local && starts_with(roaming, profile) && starts_with(local, profile));

    WCHAR buf[MAX_PATH], sysdir[MAX_PATH];
    check("CSIDL_APPDATA agrees", SHGetFolderPathW(NULL, CSIDL_APPDATA | CSIDL_FLAG_CREATE, NULL, SHGFP_TYPE_CURRENT, buf) == S_OK &&
                                      roaming && wide_eq(buf, roaming));
    check("CSIDL_LOCAL_APPDATA agrees", SHGetFolderPathW(NULL, CSIDL_LOCAL_APPDATA, NULL, SHGFP_TYPE_CURRENT, buf) == S_OK &&
                                            local && wide_eq(buf, local));
    check("CSIDL_PROFILE agrees", SHGetFolderPathW(NULL, CSIDL_PROFILE, NULL, SHGFP_TYPE_CURRENT, buf) == S_OK && profile && wide_eq(buf, profile));
    GetSystemDirectoryW(sysdir, MAX_PATH);
    check("System folder is the system directory",
          SHGetKnownFolderPath(&ID_System, KF_FLAG_DONT_VERIFY, NULL, &sys) == S_OK && sys && wide_eq(sys, sysdir));

    WCHAR *none = (WCHAR *)1;
    HRESULT hr = SHGetKnownFolderPath(&ID_Unknown, 0, NULL, &none);
    check_eq("an unknown folder id", (DWORD)hr, 0x80070002);
    check("no path for an unknown id", none == NULL);
    check_eq("an unknown CSIDL", (DWORD)SHGetFolderPathW(NULL, 0x7f, NULL, SHGFP_TYPE_CURRENT, buf), (DWORD)E_INVALIDARG);

    if (native) {
        /* A fresh OSyx profile has no Videos folder. */
        WCHAR *v = (WCHAR *)1;
        check_eq("a missing folder", (DWORD)SHGetKnownFolderPath(&ID_Videos, 0, NULL, &v), 0x80070002);
        check("no path for a missing folder", v == NULL);
        check("a missing folder, not verified", SHGetKnownFolderPath(&ID_Videos, KF_FLAG_DONT_VERIFY, NULL, &v) == S_OK && v &&
                                                    wide_ends(v, L"\\Videos") && !is_dir(v));
        CoTaskMemFree(v);
    }

    CoTaskMemFree(profile);
    CoTaskMemFree(roaming);
    CoTaskMemFree(local);
    CoTaskMemFree(sys);
    CoTaskMemFree(NULL);

    char *m = CoTaskMemAlloc(16);
    check("CoTaskMemAlloc", m != NULL);
    for (int i = 0; m && i < 16; i++) m[i] = (char)i;
    m = CoTaskMemRealloc(m, 4096);
    int kept = m != NULL;
    for (int i = 0; m && i < 16; i++) kept &= m[i] == (char)i;
    check("CoTaskMemRealloc keeps the data", kept);
    CoTaskMemFree(m);
    finish("shell");
}
