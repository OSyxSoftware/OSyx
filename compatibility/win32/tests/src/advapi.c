/* advapi32 imports of xul.dll: registry enumeration, trees, renames and
 * change notification; CryptoAPI hashes; credentials; event tracing;
 * accounts, token membership, LUIDs and the service manager. Leaves no
 * keys or credentials behind. */
#include "common.h"
#include <wincred.h>
#include <evntprov.h>
#include <sddl.h>
#include <ntsecapi.h>

static int wstr_eq(const WCHAR *a, const WCHAR *b) {
    while (*a && *a == *b) a++, b++;
    return *a == *b;
}

static const WCHAR *const KEY = L"Software\\OSyx-wintest";

void __stdcall start(void) {
    /* Registry */
    HKEY root, k;
    DWORD disp;
    RegDeleteTreeW(HKEY_CURRENT_USER, KEY);
    check("create the test key", RegCreateKeyExW(HKEY_CURRENT_USER, KEY, 0, NULL, 0, KEY_ALL_ACCESS, NULL, &root, &disp) == ERROR_SUCCESS);
    HANDLE ev = CreateEventW(NULL, FALSE, FALSE, NULL);
    check("RegNotifyChangeKeyValue (asynchronous)",
          RegNotifyChangeKeyValue(root, TRUE, REG_NOTIFY_CHANGE_NAME | REG_NOTIFY_CHANGE_LAST_SET, ev, TRUE) == ERROR_SUCCESS);
    check("... not signaled before a change", WaitForSingleObject(ev, 0) == WAIT_TIMEOUT);
    DWORD one = 1, two = 2;
    check("RegSetKeyValueW creates the subkey", RegSetKeyValueW(root, L"b", L"x", REG_DWORD, &one, 4) == ERROR_SUCCESS);
    check("... and signals the watch", WaitForSingleObject(ev, 1000) == WAIT_OBJECT_0);
    RegSetKeyValueW(root, L"a", L"y", REG_DWORD, &two, 4);
    RegSetKeyValueW(root, L"a\\deep", L"z", REG_SZ, L"hi", 6);
    RegSetValueExW(root, L"v1", 0, REG_SZ, (const BYTE *)L"one", 8);
    RegSetValueExW(root, L"v2", 0, REG_DWORD, (const BYTE *)&two, 4);
    WCHAR name[64];
    DWORD n = 64;
    int names = 0;
    for (DWORD i = 0; n = 64, RegEnumKeyExW(root, i, name, &n, NULL, NULL, NULL, NULL) == ERROR_SUCCESS; i++)
        names += wstr_eq(name, L"a") || wstr_eq(name, L"b");
    check("RegEnumKeyExW lists the two subkeys", names == 2);
    n = 64;
    check("RegEnumKeyExW past the end", RegEnumKeyExW(root, 2, name, &n, NULL, NULL, NULL, NULL) == ERROR_NO_MORE_ITEMS);
    n = 1;
    check("RegEnumKeyExW with a short buffer", RegEnumKeyExW(root, 0, name, &n, NULL, NULL, NULL, NULL) == ERROR_MORE_DATA);
    DWORD type, size, values = 0;
    BYTE data[64];
    for (DWORD i = 0;; i++) {
        n = 64, size = sizeof data;
        if (RegEnumValueW(root, i, name, &n, NULL, &type, data, &size) != ERROR_SUCCESS) break;
        values += (wstr_eq(name, L"v1") && type == REG_SZ && size == 8) || (wstr_eq(name, L"v2") && type == REG_DWORD && *(DWORD *)data == 2);
    }
    check("RegEnumValueW lists both values with data", values == 2);
    DWORD subkeys, maxsub, nvalues, maxname, maxdata;
    check("RegQueryInfoKeyW", RegQueryInfoKeyW(root, NULL, NULL, NULL, &subkeys, &maxsub, NULL, &nvalues, &maxname, &maxdata, NULL, NULL) == ERROR_SUCCESS &&
                                  subkeys == 2 && maxsub == 1 && nvalues == 2 && maxname == 2 && maxdata == 8);
    check("RegRenameKey", RegRenameKey(root, L"a", L"c") == ERROR_SUCCESS);
    size = sizeof data;
    check("... moves the subtree", RegGetValueW(root, L"c\\deep", L"z", RRF_RT_REG_SZ, NULL, data, &size) == ERROR_SUCCESS &&
                                       RegOpenKeyExW(root, L"a", 0, KEY_READ, &k) == ERROR_FILE_NOT_FOUND);
    check("RegDeleteKeyValueW", RegDeleteKeyValueW(root, L"b", L"x") == ERROR_SUCCESS &&
                                    RegDeleteKeyValueW(root, L"b", L"x") == ERROR_FILE_NOT_FOUND);
    check("RegDeleteTreeW of a subtree", RegDeleteTreeW(root, L"c") == ERROR_SUCCESS && RegOpenKeyExW(root, L"c\\deep", 0, KEY_READ, &k) == ERROR_FILE_NOT_FOUND);
    check("RegDeleteTreeW without a subkey empties the key",
          RegDeleteTreeW(root, NULL) == ERROR_SUCCESS && RegQueryInfoKeyW(root, NULL, NULL, NULL, &subkeys, NULL, NULL, &nvalues, NULL, NULL, NULL, NULL) == ERROR_SUCCESS &&
              subkeys == 0 && nvalues == 0);
    RegCloseKey(root);
    RegDeleteKeyW(HKEY_CURRENT_USER, KEY);
    CloseHandle(ev);

    /* CryptoAPI hashes */
    HCRYPTPROV prov;
    check("CryptAcquireContextW (verify context)", CryptAcquireContextW(&prov, NULL, NULL, PROV_RSA_AES, CRYPT_VERIFYCONTEXT));
    static const struct { ALG_ID alg; DWORD len; BYTE first, last; } want[] = {
        {CALG_MD5, 16, 0x90, 0x72}, {CALG_SHA1, 20, 0xa9, 0x9d}, {CALG_SHA_256, 32, 0xba, 0xad}};
    for (int i = 0; i < 3; i++) {
        HCRYPTHASH h;
        BYTE v[32];
        DWORD vl = sizeof v, hs = 0, hl = 4;
        int ok = CryptCreateHash(prov, want[i].alg, 0, 0, &h) && CryptHashData(h, (const BYTE *)"abc", 3, 0) &&
                 CryptGetHashParam(h, HP_HASHSIZE, (BYTE *)&hs, &hl, 0) && hs == want[i].len &&
                 CryptGetHashParam(h, HP_HASHVAL, v, &vl, 0) && vl == want[i].len && v[0] == want[i].first && v[vl - 1] == want[i].last;
        check(i == 0 ? "MD5(\"abc\")" : i == 1 ? "SHA-1(\"abc\")" : "SHA-256(\"abc\")", ok);
        check("... no more data after the value", !CryptHashData(h, (const BYTE *)"x", 1, 0));
        CryptDestroyHash(h);
    }
    CryptReleaseContext(prov, 0);

    /* Credentials */
    CREDENTIALA c = {0};
    c.Type = CRED_TYPE_GENERIC;
    c.TargetName = "osyx-wintest-credential";
    c.UserName = "tester";
    c.CredentialBlob = (BYTE *)"s3cret";
    c.CredentialBlobSize = 6;
    c.Persist = CRED_PERSIST_LOCAL_MACHINE;
    check("CredWriteA", CredWriteA(&c, 0));
    PCREDENTIALA got = NULL;
    check("CredReadA", CredReadA("osyx-wintest-credential", CRED_TYPE_GENERIC, 0, &got) && got->CredentialBlobSize == 6 &&
                           mem_eq(got->CredentialBlob, "s3cret", 6) && got->UserName && got->UserName[0] == 't');
    CredFree(got);
    check("CredDeleteA", CredDeleteA("osyx-wintest-credential", CRED_TYPE_GENERIC, 0));
    check("... and it is gone", !CredReadA("osyx-wintest-credential", CRED_TYPE_GENERIC, 0, &got) && GetLastError() == ERROR_NOT_FOUND);

    /* Event tracing */
    static const GUID provider = {0x6f0c1a2b, 0x1234, 0x4abc, {0x80, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07}};
    REGHANDLE reg = 0;
    check("EventRegister", EventRegister(&provider, NULL, NULL, &reg) == ERROR_SUCCESS && reg != 0);
    EVENT_DESCRIPTOR d = {1, 0, 0, 4, 0, 0, 0};
    check("EventWrite with no session listening", EventWrite(reg, &d, 0, NULL) == ERROR_SUCCESS);
    check("EventUnregister", EventUnregister(reg) == ERROR_SUCCESS);

    /* Accounts */
    WCHAR user[128];
    DWORD ul = 128;
    check("GetUserNameW", GetUserNameW(user, &ul) && ul > 1 && user[ul - 1] == 0);
    BYTE sid[SECURITY_MAX_SID_SIZE];
    WCHAR dom[64];
    DWORD sl = sizeof sid, dl = 64;
    SID_NAME_USE use;
    check("LookupAccountNameW of the current user", LookupAccountNameW(NULL, user, sid, &sl, dom, &dl, &use) && use == SidTypeUser && IsValidSid(sid));
    char *text = NULL;
    check("ConvertSidToStringSidA", ConvertSidToStringSidA(sid, &text) && text[0] == 'S' && text[1] == '-');
    LocalFree(text);
    BYTE everyone[SECURITY_MAX_SID_SIZE];
    DWORD el = sizeof everyone;
    BOOL member = FALSE;
    check("CheckTokenMembership(Everyone)", CreateWellKnownSid(WinWorldSid, NULL, everyone, &el) &&
                                                CheckTokenMembership(NULL, everyone, &member) && member);
    LUID a, b;
    check("AllocateLocallyUniqueId", AllocateLocallyUniqueId(&a) && AllocateLocallyUniqueId(&b) &&
                                          (a.LowPart != b.LowPart || a.HighPart != b.HighPart));
    SC_HANDLE scm = OpenSCManagerW(NULL, NULL, SC_MANAGER_CONNECT);
    check("OpenSCManagerW", scm != NULL);
    check("OpenServiceW of an unknown service", OpenServiceW(scm, L"osyx-no-such-service", SERVICE_QUERY_STATUS) == NULL &&
                                                   GetLastError() == ERROR_SERVICE_DOES_NOT_EXIST);
    check("CloseServiceHandle", CloseServiceHandle(scm));
    check("LsaNtStatusToWinError", LsaNtStatusToWinError(0xC0000022) == ERROR_ACCESS_DENIED);
    finish("advapi");
}
