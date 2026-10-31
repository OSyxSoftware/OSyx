/* The libraries added later: bcrypt (MD5, SHA-1, SHA-256, HMAC, random),
 * secur32 (GetUserNameExW), shfolder, winspool (no printers), uiautomationcore
 * (no UI Automation client), oleacc and msvcrt. Expected values of the
 * digests are the published ones; statuses were probed on Windows 11. */
#include "common.h"
#include <bcrypt.h>
#define SECURITY_WIN32
#include <security.h>
#include <winspool.h>
#include <shlobj.h>

#define STATUS_INVALID_HANDLE_ ((NTSTATUS)0xC0000008)
#define STATUS_INVALID_PARAMETER_ ((NTSTATUS)0xC000000D)
#define STATUS_NOT_FOUND_ ((NTSTATUS)0xC0000225)
#define STATUS_NOT_SUPPORTED_ ((NTSTATUS)0xC00000BB)

static void hex(const UCHAR *d, ULONG n, char *out) {
    static const char digits[] = "0123456789abcdef";
    for (ULONG i = 0; i < n; i++) { out[2 * i] = digits[d[i] >> 4]; out[2 * i + 1] = digits[d[i] & 15]; }
    out[2 * n] = 0;
}

static int same(const char *a, const char *b) {
    while (*a && *a == *b) { a++; b++; }
    return *a == *b;
}

/* One digest (HMAC when `key` is set) in a single pass. */
static NTSTATUS digest(const WCHAR *alg, const UCHAR *key, ULONG key_len, const char *data, ULONG len, UCHAR *out, ULONG out_len) {
    BCRYPT_ALG_HANDLE a = NULL;
    BCRYPT_HASH_HANDLE h = NULL;
    NTSTATUS s = BCryptOpenAlgorithmProvider(&a, alg, NULL, key ? BCRYPT_ALG_HANDLE_HMAC_FLAG : 0);
    if (s) return s;
    s = BCryptCreateHash(a, &h, NULL, 0, (PUCHAR)key, key_len, 0);
    if (!s) s = BCryptHashData(h, (PUCHAR)data, len, 0);
    if (!s) s = BCryptFinishHash(h, out, out_len, 0);
    if (h) BCryptDestroyHash(h);
    BCryptCloseAlgorithmProvider(a, 0);
    return s;
}

static void hashes(void) {
    UCHAR d[32];
    char text[80];
    check("SHA256(abc)", digest(L"SHA256", NULL, 0, "abc", 3, d, 32) == 0 && (hex(d, 32, text), same(text, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")));
    check("SHA256 of nothing", digest(L"SHA256", NULL, 0, "", 0, d, 32) == 0 && (hex(d, 32, text), same(text, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")));
    check("SHA1(abc)", digest(L"SHA1", NULL, 0, "abc", 3, d, 20) == 0 && (hex(d, 20, text), same(text, "a9993e364706816aba3e25717850c26c9cd0d89d")));
    check("MD5(abc)", digest(L"MD5", NULL, 0, "abc", 3, d, 16) == 0 && (hex(d, 16, text), same(text, "900150983cd24fb0d6963f7d28e17f72")));
    check("algorithm names are case-insensitive", digest(L"sha256", NULL, 0, "abc", 3, d, 32) == 0);

    static const char fox[] = "The quick brown fox jumps over the lazy dog";
    check("HMAC-SHA256", digest(L"SHA256", (const UCHAR *)"key", 3, fox, 43, d, 32) == 0 && (hex(d, 32, text), same(text, "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8")));
    check("HMAC-SHA1", digest(L"SHA1", (const UCHAR *)"key", 3, fox, 43, d, 20) == 0 && (hex(d, 20, text), same(text, "de7c9b85b8b78aa6bc8a7a36f70a90701c9db4d9")));
    check("HMAC-MD5", digest(L"MD5", (const UCHAR *)"key", 3, fox, 43, d, 16) == 0 && (hex(d, 16, text), same(text, "80070713463e7749b90c2dc24911e275")));

    /* A key longer than the block is hashed first. */
    UCHAR long_key[100], hashed_key[32], first[32], second[32];
    for (int i = 0; i < 100; i++) long_key[i] = (UCHAR)i;
    digest(L"SHA256", NULL, 0, (const char *)long_key, 100, hashed_key, 32);
    int hmac_equal = digest(L"SHA256", long_key, 100, fox, 43, first, 32) == 0 && digest(L"SHA256", hashed_key, 32, fox, 43, second, 32) == 0;
    for (int i = 0; i < 32; i++) hmac_equal &= first[i] == second[i];
    check("HMAC with a key over 64 bytes equals HMAC with its digest as the key", hmac_equal);

    /* Data in pieces, 1000 bytes, equals the same data at once. */
    char big[1000];
    for (int i = 0; i < 1000; i++) big[i] = (char)('a' + i % 26);
    BCRYPT_ALG_HANDLE a; BCRYPT_HASH_HANDLE h;
    BCryptOpenAlgorithmProvider(&a, L"SHA256", NULL, 0);
    BCryptCreateHash(a, &h, NULL, 0, NULL, 0, 0);
    for (int i = 0; i < 1000; i += 7) BCryptHashData(h, (PUCHAR)big + i, i + 7 <= 1000 ? 7 : 1000 - i, 0);
    BCryptFinishHash(h, first, 32, 0);
    digest(L"SHA256", NULL, 0, big, 1000, second, 32);
    int equal = 1;
    for (int i = 0; i < 32; i++) equal &= first[i] == second[i];
    check("hashing in pieces equals hashing at once", equal);

    /* Errors, as Windows reports them. */
    ULONG length = 0, got = 0;
    check("HashDigestLength", BCryptGetProperty(a, BCRYPT_HASH_LENGTH, (PUCHAR)&length, 4, &got, 0) == 0 && length == 32 && got == 4);
    WCHAR name[16];
    check("AlgorithmName", BCryptGetProperty(a, BCRYPT_ALGORITHM_NAME, (PUCHAR)name, sizeof name, &got, 0) == 0 && name[0] == 'S' && name[6] == 0 && got == 14);
    check("ObjectLength is a size to allocate", BCryptGetProperty(a, BCRYPT_OBJECT_LENGTH, (PUCHAR)&length, 4, &got, 0) == 0 && length > 0);
    BCryptDestroyHash(h);
    BCryptCreateHash(a, &h, NULL, 0, NULL, 0, 0);
    BCryptHashData(h, (PUCHAR)"abc", 3, 0);
    check("FinishHash with a wrong size is STATUS_INVALID_PARAMETER", BCryptFinishHash(h, first, 31, 0) == STATUS_INVALID_PARAMETER_);
    check("FinishHash with the right size", BCryptFinishHash(h, first, 32, 0) == 0);
    check("a finished hash refuses more data", BCryptHashData(h, (PUCHAR)"x", 1, 0) == STATUS_INVALID_HANDLE_);
    check("a finished hash refuses a second FinishHash", BCryptFinishHash(h, first, 32, 0) == STATUS_INVALID_HANDLE_);
    check("DestroyHash still works after FinishHash", BCryptDestroyHash(h) == 0);
    check("DestroyHash of a destroyed hash is STATUS_INVALID_PARAMETER", BCryptDestroyHash(h) == STATUS_INVALID_PARAMETER_);
    BCryptCloseAlgorithmProvider(a, 0);

    BCRYPT_ALG_HANDLE x;
    check("an unknown algorithm is STATUS_NOT_FOUND", BCryptOpenAlgorithmProvider(&x, L"NOSUCH", NULL, 0) == STATUS_NOT_FOUND_);
    check("an unknown flag is STATUS_INVALID_PARAMETER", BCryptOpenAlgorithmProvider(&x, L"SHA256", NULL, 0x80) == STATUS_INVALID_PARAMETER_);
    check("an unknown implementation is STATUS_NOT_FOUND", BCryptOpenAlgorithmProvider(&x, L"SHA256", L"Nope", 0) == STATUS_NOT_FOUND_);
    check("AES is refused as STATUS_NOT_SUPPORTED (OSyx has no ciphers; Windows would open it)", BCryptOpenAlgorithmProvider(&x, L"AES", NULL, 0) == STATUS_NOT_SUPPORTED_);
    check("SHA512 is refused as STATUS_NOT_SUPPORTED (not implemented)", BCryptOpenAlgorithmProvider(&x, L"SHA512", NULL, 0) == STATUS_NOT_SUPPORTED_);

    /* Reusable hashes start over after FinishHash. */
    BCryptOpenAlgorithmProvider(&a, L"SHA256", NULL, BCRYPT_HASH_REUSABLE_FLAG);
    BCryptCreateHash(a, &h, NULL, 0, NULL, 0, 0);
    BCryptHashData(h, (PUCHAR)"abc", 3, 0);
    BCryptFinishHash(h, first, 32, 0);
    BCryptHashData(h, (PUCHAR)"abc", 3, 0);
    check("a reusable hash gives the same digest again", BCryptFinishHash(h, second, 32, 0) == 0 && (hex(second, 32, text), same(text, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")));
    BCryptDestroyHash(h);
    check("BCryptHash in one call", BCryptHash(a, NULL, 0, (PUCHAR)"abc", 3, second, 32) == 0 && (hex(second, 32, text), same(text, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")));
    BCryptCloseAlgorithmProvider(a, 0);
}

static void randomness(void) {
    UCHAR one[32] = { 0 }, two[32] = { 0 };
    check("BCryptGenRandom with the system RNG", BCryptGenRandom(NULL, one, 32, BCRYPT_USE_SYSTEM_PREFERRED_RNG) == 0 && BCryptGenRandom(NULL, two, 32, BCRYPT_USE_SYSTEM_PREFERRED_RNG) == 0);
    int zero = 1, equal = 1;
    for (int i = 0; i < 32; i++) { zero &= one[i] == 0; equal &= one[i] == two[i]; }
    check("the random bytes are neither all zero nor repeated", !zero && !equal);
    BCRYPT_ALG_HANDLE rng;
    check("the RNG provider", BCryptOpenAlgorithmProvider(&rng, BCRYPT_RNG_ALGORITHM, NULL, 0) == 0 && BCryptGenRandom(rng, one, 16, 0) == 0);
    check("a flag with a handle is refused", BCryptGenRandom(rng, one, 16, BCRYPT_USE_SYSTEM_PREFERRED_RNG) == STATUS_INVALID_PARAMETER_);
    BCryptCloseAlgorithmProvider(rng, 0);
}

static void secur(void) {
    WCHAR small[4], big[256];
    ULONG n = 4;
    SetLastError(0);
    check("GetUserNameExW with a small buffer fails with ERROR_MORE_DATA and the size needed", !GetUserNameExW(NameSamCompatible, small, &n) && GetLastError() == ERROR_MORE_DATA && n > 4);
    ULONG need = n;
    n = 256;
    BOOL ok = GetUserNameExW(NameSamCompatible, big, &n);
    int slash = 0;
    for (ULONG i = 0; ok && big[i]; i++) slash |= big[i] == 0x5c;
    check("NameSamCompatible is COMPUTER\\user and *size excludes the NUL", ok && slash && n + 1 == need && big[n] == 0);
    n = 256;
    check("NameDisplay is the user name", GetUserNameExW(NameDisplay, big, &n) && n > 0);
    n = 256;
    SetLastError(0);
    check("NameUserPrincipal of a local account fails with ERROR_NONE_MAPPED", !GetUserNameExW(NameUserPrincipal, big, &n) && GetLastError() == ERROR_NONE_MAPPED);
    n = 256;
    SetLastError(0);
    check("an unknown format fails with ERROR_INVALID_PARAMETER", !GetUserNameExW((EXTENDED_NAME_FORMAT)99, big, &n) && GetLastError() == ERROR_INVALID_PARAMETER);
    PSecurityFunctionTableW table = InitSecurityInterfaceW();
    check("InitSecurityInterfaceW returns a function table", table != NULL && table->dwVersion >= 1 && table->AcquireCredentialsHandleW != NULL);
    CredHandle cred;
    SECURITY_STATUS status = table ? table->AcquireCredentialsHandleW(NULL, (SEC_WCHAR *)L"NoSuchPackage", SECPKG_CRED_OUTBOUND, NULL, NULL, NULL, NULL, &cred, NULL) : 0;
    check("a credential handle for an unknown package fails", status < 0);
}

static void shell_and_print(void) {
    HMODULE folder = LoadLibraryA("shfolder.dll");
    HRESULT (WINAPI *w)(HWND, int, HANDLE, DWORD, LPWSTR) = folder ? (void *)GetProcAddress(folder, "SHGetFolderPathW") : NULL;
    HRESULT (WINAPI *a)(HWND, int, HANDLE, DWORD, LPSTR) = folder ? (void *)GetProcAddress(folder, "SHGetFolderPathA") : NULL;
    WCHAR wide[MAX_PATH], direct[MAX_PATH];
    char narrow[MAX_PATH];
    check("shfolder.dll exports SHGetFolderPathW and A", w && a);
    int agree = w && w(NULL, CSIDL_APPDATA, NULL, SHGFP_TYPE_CURRENT, wide) == S_OK && SHGetFolderPathW(NULL, CSIDL_APPDATA, NULL, SHGFP_TYPE_CURRENT, direct) == S_OK;
    for (int i = 0; agree; i++) { agree &= wide[i] == direct[i]; if (!wide[i]) break; }
    check("shfolder's SHGetFolderPathW agrees with shell32's", agree);
    check("SHGetFolderPathA gives the same path as text", a && a(NULL, CSIDL_APPDATA, NULL, SHGFP_TYPE_CURRENT, narrow) == S_OK && narrow[0] == (char)wide[0] && narrow[3] == (char)wide[3]);

    HANDLE printer = (HANDLE)1;
    DWORD needed = 99, returned = 99;
    SetLastError(0);
    check("OpenPrinterW of a name fails with ERROR_INVALID_PRINTER_NAME (no printers)", !OpenPrinterW(L"Nope", &printer, NULL) && GetLastError() == ERROR_INVALID_PRINTER_NAME);
    check("EnumPrintersW lists no printers", EnumPrintersW(PRINTER_ENUM_LOCAL, NULL, 2, NULL, 0, &needed, &returned) && needed == 0 && returned == 0);
    SetLastError(0);
    check("EnumPrintersW with an unknown level fails with ERROR_INVALID_LEVEL", !EnumPrintersW(PRINTER_ENUM_LOCAL, NULL, 99, NULL, 0, &needed, &returned) && GetLastError() == ERROR_INVALID_LEVEL);
    SetLastError(0);
    check("ClosePrinter of a bogus handle fails with ERROR_INVALID_HANDLE", !ClosePrinter((HANDLE)0x1234) && GetLastError() == ERROR_INVALID_HANDLE);
}

typedef struct { void **vtable; } SimpleUnknown;

static void accessibility_and_crt(void) {
    HMODULE uia = LoadLibraryA("uiautomationcore.dll");
    HRESULT (WINAPI *not_supported)(void **) = uia ? (void *)GetProcAddress(uia, "UiaGetReservedNotSupportedValue") : NULL;
    HRESULT (WINAPI *mixed)(void **) = uia ? (void *)GetProcAddress(uia, "UiaGetReservedMixedAttributeValue") : NULL;
    BOOL (WINAPI *listening)(void) = uia ? (void *)GetProcAddress(uia, "UiaClientsAreListening") : NULL;
    check("uiautomationcore.dll loads and exports its functions", not_supported && mixed && listening);
    if (!(not_supported && mixed && listening)) return;
    check("no UI Automation client is listening", !listening());
    SimpleUnknown *u = NULL, *u2 = NULL, *m = NULL;
    check("the reserved values are two different singletons", not_supported((void **)&u) == S_OK && not_supported((void **)&u2) == S_OK && mixed((void **)&m) == S_OK && u == u2 && u != m);
    void *iface = NULL;
    static const GUID unknown_iid = { 0, 0, 0, { 0xC0, 0, 0, 0, 0, 0, 0, 0x46 } };
    static const GUID other_iid = { 0x12345678, 0, 0, { 0xC0, 0, 0, 0, 0, 0, 0, 0x46 } };
    HRESULT (WINAPI *query)(SimpleUnknown *, const GUID *, void **) = (void *)u->vtable[0];
    check("QueryInterface(IUnknown) returns the object", query(u, &unknown_iid, &iface) == S_OK && iface == u);
    check("QueryInterface of another interface is E_NOINTERFACE", query(u, &other_iid, &iface) == E_NOINTERFACE && iface == NULL);

    HMODULE acc = LoadLibraryA("oleacc.dll");
    HRESULT (WINAPI *create)(HWND, LONG, const GUID *, void **) = acc ? (void *)GetProcAddress(acc, "CreateStdAccessibleObject") : NULL;
    void *object = (void *)1;
    check("CreateStdAccessibleObject is E_NOTIMPL and clears the result", create && create(NULL, 0, &unknown_iid, &object) == E_NOTIMPL && object == NULL);

    HMODULE crt = LoadLibraryA("msvcrt.dll");
    void *(__cdecl *malloc_fn)(size_t) = crt ? (void *)GetProcAddress(crt, "malloc") : NULL;
    void (__cdecl *free_fn)(void *) = crt ? (void *)GetProcAddress(crt, "free") : NULL;
    void *(__cdecl *op_new)(size_t) = crt ? (void *)GetProcAddress(crt, "??2@YAPEAX_K@Z") : NULL;
    void (__cdecl *op_delete)(void *) = crt ? (void *)GetProcAddress(crt, "??3@YAXPEAX@Z") : NULL;
    check("msvcrt.dll exports malloc, free and the C++ allocation operators", malloc_fn && free_fn && op_new && op_delete);
    if (malloc_fn && free_fn && op_new && op_delete) {
        char *block = malloc_fn(32), *object2 = op_new(48);
        if (block) block[31] = 1;
        if (object2) object2[47] = 2;
        check("the allocated blocks can be used and freed", block && object2 && block[31] == 1 && object2[47] == 2);
        free_fn(block);
        op_delete(object2);
    }
}

void __stdcall start(void) {
    hashes();
    randomness();
    secur();
    shell_and_print();
    accessibility_and_crt();
    finish("newlibs");
}
