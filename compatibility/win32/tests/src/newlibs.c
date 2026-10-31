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
#include <objidl.h>

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

/* ---- winmm devices, raw input and touch, IP interfaces, memory streams ---- */

static void devices_and_input(void) {
    char buf[64];
    UINT size = 64, count = 7;
    HMODULE winmm = LoadLibraryA("winmm.dll"), user = LoadLibraryA("user32.dll");
    UINT (WINAPI *wave_count)(void) = (void *)GetProcAddress(winmm, "waveOutGetNumDevs");
    UINT (WINAPI *midi_out_count)(void) = (void *)GetProcAddress(winmm, "midiOutGetNumDevs");
    UINT (WINAPI *midi_in_count)(void) = (void *)GetProcAddress(winmm, "midiInGetNumDevs");
    UINT (WINAPI *wave_in_count)(void) = (void *)GetProcAddress(winmm, "waveInGetNumDevs");
    UINT (WINAPI *wave_caps)(UINT_PTR, void *, UINT) = (void *)GetProcAddress(winmm, "waveOutGetDevCapsW");
    UINT (WINAPI *wave_open)(void *, UINT, const void *, DWORD_PTR, DWORD_PTR, DWORD) = (void *)GetProcAddress(winmm, "waveOutOpen");
    UINT (WINAPI *wave_close)(HANDLE) = (void *)GetProcAddress(winmm, "waveOutClose");
    UINT (WINAPI *wave_prepare)(HANDLE, void *, UINT) = (void *)GetProcAddress(winmm, "waveOutPrepareHeader");
    UINT (WINAPI *wave_unprepare)(HANDLE, void *, UINT) = (void *)GetProcAddress(winmm, "waveOutUnprepareHeader");
    UINT (WINAPI *wave_write)(HANDLE, void *, UINT) = (void *)GetProcAddress(winmm, "waveOutWrite");
    UINT (WINAPI *wave_pause)(HANDLE) = (void *)GetProcAddress(winmm, "waveOutPause");
    UINT (WINAPI *wave_restart)(HANDLE) = (void *)GetProcAddress(winmm, "waveOutRestart");
    UINT (WINAPI *wave_reset)(HANDLE) = (void *)GetProcAddress(winmm, "waveOutReset");
    UINT (WINAPI *wave_position)(HANDLE, void *, UINT) = (void *)GetProcAddress(winmm, "waveOutGetPosition");
    UINT (WINAPI *wave_text)(UINT, WCHAR *, UINT) = (void *)GetProcAddress(winmm, "waveOutGetErrorTextW");
    UINT (WINAPI *midi_short)(HANDLE, DWORD) = (void *)GetProcAddress(winmm, "midiOutShortMsg");
    UINT (WINAPI *midi_start)(HANDLE) = (void *)GetProcAddress(winmm, "midiInStart");
    UINT (WINAPI *midi_open)(void *, UINT, DWORD_PTR, DWORD_PTR, DWORD) = (void *)GetProcAddress(winmm, "midiOutOpen");
    check("winmm exports the wave and MIDI entry points", wave_count && midi_out_count && midi_in_count && wave_in_count && wave_caps && wave_open && wave_close && wave_prepare && wave_write && wave_reset && wave_position);
    HANDLE opened = NULL;
    UINT cards = wave_count ? wave_count() : 0;
    if (cards == 0) {
        check("with no sound card the id names no device: MMSYSERR_BADDEVICEID", wave_caps(0, buf, 8) == 2 && wave_open(&opened, 0, buf, 0, 0, 0) == 2 && opened == NULL);
    } else {
        unsigned char caps[84] = { 0 };
        check("waveOutGetDevCapsW names the card and its two channels", wave_caps(0, caps, 84) == 0 && *(WCHAR *)(caps + 8) != 0 && *(WORD *)(caps + 76) == 2);
        WAVEFORMATEX bad = { 1, 2, 48000, 192000, 4, 24, 0 };
        check("a 24-bit format is WAVERR_BADFORMAT", wave_open(&opened, 0, &bad, 0, 0, 0) == 32 && opened == NULL);
        WAVEFORMATEX fmt = { 1, 2, 44100, 176400, 4, 16, 0 };
        check("WAVE_FORMAT_QUERY accepts 44.1 kHz stereo without opening", wave_open(NULL, 0, &fmt, 0, 0, 1) == 0);
        HANDLE event = CreateEventW(NULL, FALSE, FALSE, NULL);
        check("waveOutOpen with CALLBACK_EVENT returns a handle", wave_open(&opened, 0xFFFFFFFF, &fmt, (DWORD_PTR)event, 0, 0x50000) == 0 && opened != NULL);
        static short silence[441 * 2];
        unsigned char hdr[48] = { 0 };
        *(void **)hdr = silence;
        *(DWORD *)(hdr + 8) = sizeof silence;
        check("waveOutWrite of an unprepared header is WAVERR_UNPREPARED", wave_write(opened, hdr, 48) == 34);
        check("waveOutPrepareHeader sets WHDR_PREPARED", wave_prepare(opened, hdr, 48) == 0 && (*(DWORD *)(hdr + 24) & 2));
        check("waveOutWrite queues the header: WHDR_INQUEUE or already done", wave_write(opened, hdr, 48) == 0);
        DWORD waited = WaitForSingleObject(event, 5000);
        check("the callback event fires and the header is WHDR_DONE", waited == 0 && (*(DWORD *)(hdr + 24) & 1) && !(*(DWORD *)(hdr + 24) & 0x10));
        unsigned char time[16] = { 0 };
        *(DWORD *)time = 4;
        check("waveOutGetPosition in bytes counts what was played", wave_position(opened, time, 12) == 0 && *(DWORD *)time == 4 && *(DWORD *)(time + 4) == sizeof silence);
        *(DWORD *)time = 1;
        check("waveOutGetPosition in milliseconds", wave_position(opened, time, 12) == 0 && *(DWORD *)time == 1 && *(DWORD *)(time + 4) == 10);
        check("waveOutPause and waveOutRestart succeed", wave_pause(opened) == 0 && wave_restart(opened) == 0);
        check("waveOutReset rewinds the position", wave_reset(opened) == 0 && (*(DWORD *)time = 4, wave_position(opened, time, 12) == 0) && *(DWORD *)(time + 4) == 0);
        check("waveOutUnprepareHeader clears WHDR_PREPARED", wave_unprepare(opened, hdr, 48) == 0 && !(*(DWORD *)(hdr + 24) & 2));
        check("waveOutClose releases the handle; it is invalid after", wave_close(opened) == 0 && wave_close(opened) == 5);
        CloseHandle(event);
    }
    check("a wave handle that was never opened is MMSYSERR_INVALHANDLE", wave_close((HANDLE)0x1234) == 5);
    check("a wave header of the wrong size is MMSYSERR_INVALPARAM", wave_write((HANDLE)0x1234, buf, 32) == 11);
    WCHAR text[128];
    check("waveOutGetErrorTextW describes MMSYSERR_INVALHANDLE and rejects unknown codes", wave_text(5, text, 128) == 0 && text[0] != 0 && wave_text(999, text, 128) == 9);
    check("MIDI and capture device counts are those of the /dev nodes", midi_out_count() == midi_in_count() && wave_in_count() == wave_in_count());
    if (midi_out_count() == 0) {
        check("with no MIDI port the id names no device: MMSYSERR_BADDEVICEID", midi_open(&opened, 0, 0, 0, 0) == 2);
    }
    check("a MIDI handle that was never opened is MMSYSERR_INVALHANDLE", midi_short((HANDLE)0x1234, 0) == 5 && midi_start((HANDLE)0x1234) == 5);
    check("a short MIDI message with a data byte as status is refused for an invalid handle first", midi_short((HANDLE)0x1234, 0x40) == 5);

    UINT (WINAPI *list)(void *, UINT *, UINT) = user ? (void *)GetProcAddress(user, "GetRawInputDeviceList") : NULL;
    UINT (WINAPI *info)(HANDLE, UINT, void *, UINT *) = user ? (void *)GetProcAddress(user, "GetRawInputDeviceInfoW") : NULL;
    UINT (WINAPI *data)(HANDLE, UINT, void *, UINT *, UINT) = user ? (void *)GetProcAddress(user, "GetRawInputData") : NULL;
    BOOL (WINAPI *pointer_type)(UINT, void *) = user ? (void *)GetProcAddress(user, "GetPointerType") : NULL;
    BOOL (WINAPI *touch_info)(HANDLE, UINT, void *, int) = user ? (void *)GetProcAddress(user, "GetTouchInputInfo") : NULL;
    BOOL (WINAPI *touch_close)(HANDLE) = user ? (void *)GetProcAddress(user, "CloseTouchInputHandle") : NULL;
    BOOL (WINAPI *gesture_info)(HANDLE, void *) = user ? (void *)GetProcAddress(user, "GetGestureInfo") : NULL;
    BOOL (WINAPI *layered)(HWND, DWORD *, BYTE *, DWORD *) = user ? (void *)GetProcAddress(user, "GetLayeredWindowAttributes") : NULL;
    SetLastError(0);
    check("GetRawInputDeviceList without a count is ERROR_NOACCESS", list && list(NULL, NULL, 16) == (UINT)-1 && GetLastError() == ERROR_NOACCESS);
    SetLastError(0);
    check("GetRawInputDeviceList with a wrong element size is ERROR_INVALID_PARAMETER", list && list(NULL, &count, 4) == (UINT)-1 && GetLastError() == ERROR_INVALID_PARAMETER);
    check("GetRawInputDeviceList finds no devices", list && list(NULL, &count, 16) == 0 && count == 0);
    SetLastError(0);
    check("GetRawInputDeviceInfoW of an unknown handle is ERROR_INVALID_HANDLE", info && info((HANDLE)0x1234, 0x20000007, buf, &size) == (UINT)-1 && GetLastError() == ERROR_INVALID_HANDLE);
    SetLastError(0);
    check("GetRawInputData of an unknown handle is ERROR_INVALID_HANDLE", data && data((HANDLE)0x1234, 0x10000003, buf, &size, 24) == (UINT)-1 && GetLastError() == ERROR_INVALID_HANDLE);
    SetLastError(0);
    check("GetPointerType of an unknown pointer is ERROR_INVALID_PARAMETER", pointer_type && !pointer_type(0xFFFF, buf) && GetLastError() == ERROR_INVALID_PARAMETER);
    SetLastError(0);
    check("GetTouchInputInfo / CloseTouchInputHandle of an unknown handle are ERROR_INVALID_HANDLE", touch_info && !touch_info((HANDLE)0x1234, 1, buf, 40) && GetLastError() == ERROR_INVALID_HANDLE && touch_close && !touch_close((HANDLE)0x1234) && GetLastError() == ERROR_INVALID_HANDLE);
    SetLastError(0);
    check("GetGestureInfo of an unknown handle is ERROR_INVALID_PARAMETER", gesture_info && !gesture_info((HANDLE)0x1234, buf) && GetLastError() == ERROR_INVALID_PARAMETER);
    SetLastError(0);
    check("GetLayeredWindowAttributes of an unknown window is ERROR_INVALID_WINDOW_HANDLE", layered && !layered((HWND)0x1234, NULL, NULL, NULL) && GetLastError() == ERROR_INVALID_WINDOW_HANDLE);
}

static void interfaces_and_streams(void) {
    HMODULE ip = LoadLibraryA("iphlpapi.dll");
    ULONG (WINAPI *interface_table)(USHORT, void **) = ip ? (void *)GetProcAddress(ip, "GetIpInterfaceTable") : NULL;
    void (WINAPI *free_table)(void *) = ip ? (void *)GetProcAddress(ip, "FreeMibTable") : NULL;
    ULONG (WINAPI *best)(const void *, DWORD *) = ip ? (void *)GetProcAddress(ip, "GetBestInterfaceEx") : NULL;
    char *(WINAPI *index_to_name)(ULONG, char *) = ip ? (void *)GetProcAddress(ip, "if_indextoname") : NULL;
    ULONG (WINAPI *name_to_index)(const char *) = ip ? (void *)GetProcAddress(ip, "if_nametoindex") : NULL;
    unsigned char *table = NULL;
    check("GetIpInterfaceTable(AF_INET) returns a table", interface_table && interface_table(2, (void **)&table) == 0 && table != NULL);
    ULONG entries = table ? *(ULONG *)table : 0;
    check("every row is AF_INET with the LUID index of its interface", entries > 0 && *(USHORT *)(table + 8) == 2 && (*(ULONGLONG *)(table + 8 + 8) >> 24 & 0xFFFFFF) == *(ULONG *)(table + 8 + 16));
    ULONG first_index = table ? *(ULONG *)(table + 8 + 16) : 0;
    if (free_table) free_table(table);
    if (free_table) free_table(NULL);
    unsigned char *empty = NULL;
    check("GetIpInterfaceTable(AF_INET6) is an empty table", interface_table && interface_table(23, (void **)&empty) == 0 && empty && *(ULONG *)empty == 0);
    if (free_table) free_table(empty);
    check("GetIpInterfaceTable rejects an unknown family and a NULL table", interface_table && interface_table(77, NULL) == ERROR_INVALID_PARAMETER && interface_table(77, (void **)&empty) == ERROR_INVALID_PARAMETER);
    unsigned char dest[16] = { 2, 0, 0, 0, 127, 0, 0, 1 };
    DWORD index = 0xFFFFFFFF;
    check("GetBestInterfaceEx rejects NULL arguments", best && best(NULL, NULL) == ERROR_INVALID_PARAMETER);
    check("GetBestInterfaceEx for 127.0.0.1 picks an interface", best && best(dest, &index) == 0 && index != 0xFFFFFFFF);
    char name[64] = { 0 };
    check("if_indextoname / if_nametoindex round trip", index_to_name && name_to_index && index_to_name(first_index, name) == name && name[0] && name_to_index(name) == first_index);
    check("if_indextoname of an unknown index is NULL", index_to_name && index_to_name(99999, name) == NULL);

    HMODULE ole = LoadLibraryA("ole32.dll");
    HRESULT (WINAPI *create)(HGLOBAL, BOOL, IStream **) = ole ? (void *)GetProcAddress(ole, "CreateStreamOnHGlobal") : NULL;
    IStream *s = NULL, *c = NULL, *d = NULL;
    char buf[16] = { 0 };
    ULONG n = 0;
    LARGE_INTEGER zero = { 0 }, minus = { 0 };
    ULARGE_INTEGER pos, size, rd, wr;
    minus.QuadPart = -1;
    check("CreateStreamOnHGlobal(NULL) makes a stream; a NULL out is E_INVALIDARG", create && create(NULL, TRUE, &s) == S_OK && s && create(NULL, TRUE, NULL) == E_INVALIDARG);
    if (!s) return;
    check("Write stores the bytes", s->lpVtbl->Write(s, "abcd", 4, &n) == S_OK && n == 4);
    check("Seek(0, SET) rewinds", s->lpVtbl->Seek(s, zero, STREAM_SEEK_SET, &pos) == S_OK && pos.QuadPart == 0);
    check("Read returns the bytes and S_OK even past the end", s->lpVtbl->Read(s, buf, 10, &n) == S_OK && n == 4 && buf[0] == 'a' && buf[3] == 'd' && s->lpVtbl->Read(s, buf, 10, &n) == S_OK && n == 0);
    check("a negative position or an unknown origin is STG_E_SEEKERROR", s->lpVtbl->Seek(s, minus, STREAM_SEEK_SET, &pos) == STG_E_SEEKERROR && s->lpVtbl->Seek(s, zero, 7, &pos) == STG_E_SEEKERROR);
    minus.QuadPart = 100;
    check("writing beyond the end grows the stream with zeros", s->lpVtbl->Seek(s, minus, STREAM_SEEK_SET, &pos) == S_OK && pos.QuadPart == 100 && s->lpVtbl->Write(s, "x", 1, &n) == S_OK && s->lpVtbl->Seek(s, zero, STREAM_SEEK_END, &pos) == S_OK && pos.QuadPart == 101);
    size.QuadPart = 2;
    STATSTG st;
    check("SetSize truncates; Stat reports a stream of that size", s->lpVtbl->SetSize(s, size) == S_OK && s->lpVtbl->Stat(s, &st, STATFLAG_NONAME) == S_OK && st.type == STGTY_STREAM && st.cbSize.QuadPart == 2 && st.pwcsName == NULL);
    check("Commit and Revert succeed; LockRegion is STG_E_INVALIDFUNCTION", s->lpVtbl->Commit(s, 0) == S_OK && s->lpVtbl->Revert(s) == S_OK && s->lpVtbl->LockRegion(s, pos, pos, LOCK_WRITE) == STG_E_INVALIDFUNCTION);
    check("Clone shares the bytes with its own position", s->lpVtbl->Seek(s, zero, STREAM_SEEK_SET, &pos) == S_OK && s->lpVtbl->Clone(s, &c) == S_OK && c->lpVtbl->Read(c, buf, 2, &n) == S_OK && n == 2 && c->lpVtbl->Write(c, "ZZ", 2, &n) == S_OK && s->lpVtbl->Seek(s, zero, STREAM_SEEK_SET, &pos) == S_OK && s->lpVtbl->Read(s, buf, 4, &n) == S_OK && n == 4 && buf[0] == 'a' && buf[2] == 'Z');
    size.QuadPart = 100;
    check("CopyTo copies from the position", create(NULL, TRUE, &d) == S_OK && s->lpVtbl->Seek(s, zero, STREAM_SEEK_SET, &pos) == S_OK && s->lpVtbl->CopyTo(s, d, size, &rd, &wr) == S_OK && rd.QuadPart == 4 && wr.QuadPart == 4);
    void *other = NULL;
    check("QueryInterface answers IUnknown and IStream, not IDispatch", s->lpVtbl->QueryInterface(s, &IID_IUnknown, &other) == S_OK && s->lpVtbl->QueryInterface(s, &IID_IStream, &other) == S_OK && s->lpVtbl->QueryInterface(s, &IID_IDispatch, &other) == E_NOINTERFACE && other == NULL);
    check("Release counts the extra references", s->lpVtbl->Release(s) == 2 && s->lpVtbl->Release(s) == 1 && s->lpVtbl->Release(s) == 0);
    c->lpVtbl->Release(c);
    d->lpVtbl->Release(d);
}

/* ---- thread information, WER modules, cycle time (results probed on Windows) ---- */

static void thread_wer_cycles(void) {
    HMODULE k = GetModuleHandleA("kernel32.dll");
    BOOL (WINAPI *set_info)(HANDLE, int, void *, DWORD) = (void *)GetProcAddress(k, "SetThreadInformation");
    BOOL (WINAPI *get_info)(HANDLE, int, void *, DWORD) = (void *)GetProcAddress(k, "GetThreadInformation");
    LONG (WINAPI *wer_register)(const WCHAR *, void *) = (void *)GetProcAddress(k, "WerRegisterRuntimeExceptionModule");
    LONG (WINAPI *wer_unregister)(const WCHAR *, void *) = (void *)GetProcAddress(k, "WerUnregisterRuntimeExceptionModule");
    BOOL (WINAPI *cycles)(HANDLE, ULONGLONG *) = (void *)GetProcAddress(k, "QueryProcessCycleTime");
    HANDLE t = GetCurrentThread();
    DWORD v = 3, memory = 0;
    check("kernel32 exports the thread information and WER functions", set_info && get_info && wer_register && wer_unregister && cycles);
    check("SetThreadInformation(ThreadMemoryPriority, 3) succeeds and reads back", set_info(t, 0, &v, 4) && get_info(t, 0, &memory, 4) && memory == 3);
    v = 6;
    SetLastError(0);
    check("a memory priority above 5 is ERROR_INVALID_PARAMETER", !set_info(t, 0, &v, 4) && GetLastError() == ERROR_INVALID_PARAMETER);
    v = 3;
    SetLastError(0);
    check("a wrong length is ERROR_BAD_LENGTH", !set_info(t, 0, &v, 8) && GetLastError() == 24);
    SetLastError(0);
    check("a NULL block is ERROR_NOACCESS", !set_info(t, 0, NULL, 4) && GetLastError() == ERROR_NOACCESS);
    SetLastError(0);
    check("an unknown class is ERROR_INVALID_PARAMETER", !set_info(t, 77, &v, 4) && GetLastError() == ERROR_INVALID_PARAMETER);
    SetLastError(0);
    check("a bad handle is ERROR_INVALID_HANDLE", !set_info((HANDLE)0x1234, 0, &v, 4) && GetLastError() == ERROR_INVALID_HANDLE);
    DWORD throttle[3] = { 1, 1, 1 }, back[3] = { 0, 0, 0 };
    check("power throttling is accepted and read back", set_info(t, 3, throttle, 12) && get_info(t, 3, back, 12) && back[0] == 1 && back[1] == 1 && back[2] == 1);
    throttle[0] = 2;
    SetLastError(0);
    check("power throttling version 2 is ERROR_INVALID_PARAMETER", !set_info(t, 3, throttle, 12) && GetLastError() == ERROR_INVALID_PARAMETER);

    check("WerRegisterRuntimeExceptionModule keeps a module and a context", wer_register(L"C:\\nonexistent\\y.dll", NULL) == 0 && wer_register(L"x.dll", (void *)5) == 0);
    check("a NULL module is E_INVALIDARG", wer_register(NULL, NULL) == (LONG)0x80070057);
    check("WerUnregisterRuntimeExceptionModule removes it once, then it is not found", wer_unregister(L"x.dll", (void *)5) == 0 && wer_unregister(L"x.dll", (void *)5) == (LONG)0x80070002);
    check("an unregistered module is HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND)", wer_unregister(L"never.dll", NULL) == (LONG)0x80070002);

    ULONGLONG c = 0, c2 = 0;
    SetLastError(0);
    check("QueryProcessCycleTime of a bad handle is ERROR_INVALID_HANDLE", !cycles((HANDLE)0x1234, &c) && GetLastError() == ERROR_INVALID_HANDLE);
    DWORD end = GetTickCount() + 30;
    while (GetTickCount() < end) {}
    check("QueryProcessCycleTime counts cycles of the process", cycles(GetCurrentProcess(), &c) && c > 0);
    end = GetTickCount() + 30;
    while (GetTickCount() < end) {}
    check("the cycle count grows with CPU time", cycles(GetCurrentProcess(), &c2) && c2 > c);
}

void __stdcall start(void) {
    hashes();
    randomness();
    secur();
    shell_and_print();
    accessibility_and_crt();
    devices_and_input();
    interfaces_and_streams();
    thread_wer_cycles();
    finish("newlibs");
}
