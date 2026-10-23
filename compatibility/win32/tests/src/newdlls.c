/* The smaller DLLs xul.dll links against: rpcrt4 UUID strings, cfgmgr32
 * interface lists, ktmw32, propsys property stores and conversions,
 * windowscodecs, bcryptprimitives, shcore, and the ntdll/kernelbase
 * calls (path conversion, system information, growable function tables,
 * PathCchCanonicalize). */
#include "common.h"
#include <rpc.h>
#include <cfgmgr32.h>
#include <propsys.h>
#include <propvarutil.h>

/* Not declared by the MinGW headers. */
int WINAPI VariantCompare(const VARIANT *, const VARIANT *);
#ifndef STRSAFE_E_INSUFFICIENT_BUFFER
#define STRSAFE_E_INSUFFICIENT_BUFFER ((HRESULT)0x8007007AL)
#endif

static int wstr_eq(const WCHAR *a, const WCHAR *b) {
    while (*a && *a == *b) a++, b++;
    return *a == *b;
}

static int wends(const WCHAR *s, const WCHAR *tail) {
    int n = 0, t = 0;
    while (s[n]) n++;
    while (tail[t]) t++;
    return n >= t && wstr_eq(s + n - t, tail);
}

typedef struct { USHORT Length, MaximumLength; PWSTR Buffer; } USTR;
typedef LONG(WINAPI *DosToNt)(PCWSTR, USTR *, PWSTR *, void *);
typedef LONG(WINAPI *QuerySys)(ULONG, void *, ULONG, ULONG *);
typedef LONG(WINAPI *AddGrowable)(void **, PRUNTIME_FUNCTION, DWORD, DWORD, ULONG_PTR, ULONG_PTR);
typedef void(WINAPI *DeleteGrowable)(void *);
typedef BOOL(WINAPI *Prng)(PBYTE, SIZE_T);
typedef HRESULT(WINAPI *Canon)(PWSTR, SIZE_T, PCWSTR);
typedef HRESULT(WINAPI *Dpi)(HMONITOR, int, UINT *, UINT *);
typedef HANDLE(WINAPI *CreateTx)(LPSECURITY_ATTRIBUTES, LPGUID, DWORD, DWORD, DWORD, DWORD, LPWSTR);
typedef BOOL(WINAPI *CommitTx)(HANDLE);
typedef HRESULT(WINAPI *WicConvert)(const GUID *, void *, void **);

static RUNTIME_FUNCTION functions[4];
static const IID iid_property_store = {0x886d8eeb, 0x8cf2, 0x4446, {0x8d, 0x02, 0xcd, 0xba, 0x1d, 0xbd, 0xcf, 0x99}};

void __stdcall start(void) {
    /* rpcrt4 */
    UUID u;
    check("UuidFromStringW", UuidFromStringW((RPC_WSTR)L"886d8eeb-8cf2-4446-8d02-cdba1dbdcf99", &u) == RPC_S_OK &&
                                 u.Data1 == 0x886d8eeb && u.Data2 == 0x8cf2 && u.Data4[0] == 0x8d && u.Data4[7] == 0x99);
    RPC_WSTR w = NULL;
    check("UuidToStringW round trip", UuidToStringW(&u, &w) == RPC_S_OK && wstr_eq((WCHAR *)w, L"886d8eeb-8cf2-4446-8d02-cdba1dbdcf99"));
    check("RpcStringFreeW", RpcStringFreeW(&w) == RPC_S_OK && w == NULL);
    RPC_CSTR a = NULL;
    check("UuidToStringA", UuidToStringA(&u, &a) == RPC_S_OK && a[0] == '8' && a[35] == '9' && a[36] == 0);
    RpcStringFreeA(&a);
    check("UuidFromStringW rejects junk", UuidFromStringW((RPC_WSTR)L"not-a-uuid", &u) == RPC_S_INVALID_STRING_UUID);
    check("UuidFromStringW(NULL) is the nil UUID", UuidFromStringW(NULL, &u) == RPC_S_OK && u.Data1 == 0 && u.Data4[7] == 0);

    /* cfgmgr32: an interface class nobody registers has an empty list. */
    static const GUID nobody = {0x0badc0de, 0x1, 0x2, {3, 4, 5, 6, 7, 8, 9, 10}};
    ULONG len = 0;
    check("CM_Get_Device_Interface_List_SizeW of an unused class", CM_Get_Device_Interface_List_SizeW(&len, (LPGUID)&nobody, NULL, 0) == CR_SUCCESS && len == 1);

    /* propsys */
    IPropertyStore *ps = NULL;
    check("PSCreateMemoryPropertyStore", PSCreateMemoryPropertyStore(&iid_property_store, (void **)&ps) == S_OK && ps);
    PROPERTYKEY k1 = {{0x11111111, 1, 2, {1, 2, 3, 4, 5, 6, 7, 8}}, 2}, k2 = k1;
    k2.pid = 3;
    PROPVARIANT v, out;
    v.vt = VT_LPWSTR;
    v.pwszVal = L"hello";
    check("SetValue (string)", ps->lpVtbl->SetValue(ps, &k1, &v) == S_OK);
    v.vt = VT_I4;
    v.lVal = -42;
    check("SetValue (number)", ps->lpVtbl->SetValue(ps, &k2, &v) == S_OK);
    DWORD count = 0;
    check("GetCount", ps->lpVtbl->GetCount(ps, &count) == S_OK && count == 2);
    PropVariantInit(&out);
    check("GetValue copies the string", ps->lpVtbl->GetValue(ps, &k1, &out) == S_OK && out.vt == VT_LPWSTR && wstr_eq(out.pwszVal, L"hello"));
    PropVariantClear(&out);
    WCHAR text[32];
    check("GetValue + PropVariantToString", ps->lpVtbl->GetValue(ps, &k2, &out) == S_OK && PropVariantToString(&out, text, 32) == S_OK &&
                                                wstr_eq(text, L"-42"));
    PROPERTYKEY k3 = k1;
    k3.pid = 99;
    check("an absent property is VT_EMPTY", ps->lpVtbl->GetValue(ps, &k3, &out) == S_OK && out.vt == VT_EMPTY);
    check("PropVariantToString truncates", (v.vt = VT_LPWSTR, v.pwszVal = L"abcdef", PropVariantToString(&v, text, 4) == STRSAFE_E_INSUFFICIENT_BUFFER) &&
                                               wstr_eq(text, L"abc"));
    check("Release frees the store", ps->lpVtbl->Release(ps) == 0);
    VARIANT x, y;
    VariantInit(&x), VariantInit(&y);
    x.vt = VT_I4, x.lVal = 3;
    y.vt = VT_I4, y.lVal = 5;
    check("VariantCompare numbers", VariantCompare(&x, &y) < 0 && VariantCompare(&y, &x) > 0 && VariantCompare(&x, &x) == 0);

    /* bcryptprimitives */
    HMODULE bp = LoadLibraryA("bcryptprimitives.dll");
    Prng prng = (Prng)GetProcAddress(bp, "ProcessPrng");
    BYTE r1[32] = {0}, r2[32] = {0};
    check("ProcessPrng fills two different buffers", prng && prng(r1, 32) && prng(r2, 32) && !mem_eq(r1, r2, 32));

    /* shcore: a handle that is no monitor */
    Dpi dpi = (Dpi)GetProcAddress(LoadLibraryA("shcore.dll"), "GetDpiForMonitor");
    UINT dx = 0, dy = 0;
    check("GetDpiForMonitor of a bad handle", dpi && dpi((HMONITOR)(ULONG_PTR)0x7777, 0, &dx, &dy) == HRESULT_FROM_WIN32(ERROR_INVALID_HANDLE));

    /* windowscodecs */
    WicConvert wic = (WicConvert)GetProcAddress(LoadLibraryA("windowscodecs.dll"), "WICConvertBitmapSource");
    void *dst = (void *)1;
    check("WICConvertBitmapSource without a source", wic && wic(&nobody, NULL, &dst) == E_INVALIDARG);

    /* ktmw32: a transaction either works and commits, or is unsupported. */
    HMODULE ktm = LoadLibraryA("ktmw32.dll");
    CreateTx ctx = (CreateTx)GetProcAddress(ktm, "CreateTransaction");
    CommitTx ctc = (CommitTx)GetProcAddress(ktm, "CommitTransaction");
    HANDLE tx = ctx ? ctx(NULL, NULL, 0, 0, 0, 0, NULL) : INVALID_HANDLE_VALUE;
    check("CreateTransaction works or reports it is unsupported",
          ctx && ctc && (tx != INVALID_HANDLE_VALUE ? ctc(tx) && CloseHandle(tx) : GetLastError() == ERROR_NOT_SUPPORTED));

    /* ntdll */
    HMODULE nt = GetModuleHandleA("ntdll.dll");
    DosToNt d2n = (DosToNt)GetProcAddress(nt, "RtlDosPathNameToNtPathName_U_WithStatus");
    USTR ntname = {0};
    PWSTR part = NULL;
    check("RtlDosPathNameToNtPathName_U_WithStatus", d2n && d2n(L"C:\\Windows\\..\\temp\\x.txt", &ntname, &part, NULL) == 0 &&
                                                          wstr_eq(ntname.Buffer, L"\\??\\C:\\temp\\x.txt") && part && wstr_eq(part, L"x.txt") &&
                                                          ntname.Length == 34);
    if (ntname.Buffer) HeapFree(GetProcessHeap(), 0, ntname.Buffer);
    QuerySys qsi = (QuerySys)GetProcAddress(nt, "NtQuerySystemInformation");
    BYTE basic[64];
    ULONG ret = 0;
    check("NtQuerySystemInformation(SystemBasicInformation)", qsi && qsi(0, basic, 64, &ret) == 0 && ret == 64 &&
                                                                 *(ULONG *)(basic + 8) == 4096 && basic[56] >= 1);
    check("... a wrong length is a mismatch", qsi(0, basic, 60, &ret) == (LONG)0xC0000004);
    AddGrowable add = (AddGrowable)GetProcAddress(nt, "RtlAddGrowableFunctionTable");
    DeleteGrowable del = (DeleteGrowable)GetProcAddress(nt, "RtlDeleteGrowableFunctionTable");
    static BYTE code[256];
    void *table = NULL;
    functions[0].BeginAddress = 0;
    functions[0].EndAddress = 16;
    check("RtlAddGrowableFunctionTable", add && add(&table, functions, 1, 4, (ULONG_PTR)code, (ULONG_PTR)code + sizeof code) == 0 && table);
    if (table && del) del(table);

    /* kernelbase: PathCchCanonicalize */
    Canon canon = (Canon)GetProcAddress(LoadLibraryA("kernelbase.dll"), "PathCchCanonicalize");
    WCHAR p[MAX_PATH];
    check("PathCchCanonicalize", canon && canon(p, MAX_PATH, L"C:\\a\\.\\b\\..\\c") == S_OK && wstr_eq(p, L"C:\\a\\c"));
    check("... not above the root", canon(p, MAX_PATH, L"C:\\..\\x") == S_OK && wstr_eq(p, L"C:\\x"));
    check("... too small a buffer", canon(p, 4, L"C:\\abc\\def") == HRESULT_FROM_WIN32(ERROR_INSUFFICIENT_BUFFER));
    (void)wends;
    finish("newdlls");
}
