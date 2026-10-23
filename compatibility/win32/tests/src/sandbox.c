/* advapi32 as a browser sandbox uses it: string SIDs, trustees and
 * SetEntriesInAcl, mandatory labels, object security descriptors
 * (Get/SetSecurityInfo, SetKernelObjectSecurity, named files), AccessCheck
 * with impersonation, restricted and lower-integrity tokens, privilege
 * adjustment, thread tokens and the event log. The same PE runs on
 * Windows; OSyx-specific checks are gated by the OSYX environment
 * variable. */
#include "common.h"
#include <aclapi.h>
#include <sddl.h>

static int wcs_eq(const WCHAR *a, const WCHAR *b) {
    while (*a && *a == *b) a++, b++;
    return *a == *b;
}

static BYTE world[SECURITY_MAX_SID_SIZE], users[SECURITY_MAX_SID_SIZE], admins[SECURITY_MAX_SID_SIZE];
static BYTE user_buf[256];
static GENERIC_MAPPING file_map = {FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_GENERIC_EXECUTE, FILE_ALL_ACCESS};

static PSID token_user(HANDLE token) {
    DWORD n = 0;
    if (!GetTokenInformation(token, TokenUser, user_buf, sizeof user_buf, &n)) return NULL;
    return ((TOKEN_USER *)user_buf)->User.Sid;
}

/* An absolute descriptor owned by `owner` with `dacl` (NULL DACL when dacl==NULL and present). */
static void make_sd(SECURITY_DESCRIPTOR *sd, PSID owner, PACL dacl, BOOL present) {
    InitializeSecurityDescriptor(sd, SECURITY_DESCRIPTOR_REVISION);
    SetSecurityDescriptorOwner(sd, owner, FALSE);
    SetSecurityDescriptorGroup(sd, owner, FALSE);
    SetSecurityDescriptorDacl(sd, present, dacl, FALSE);
}

static BOOL check_access(HANDLE imp, SECURITY_DESCRIPTOR *sd, DWORD desired, DWORD *granted, BOOL *status) {
    BYTE privs[64];
    DWORD plen = sizeof privs;
    *granted = 0;
    *status = FALSE;
    return AccessCheck(sd, imp, desired, &file_map, (PPRIVILEGE_SET)privs, &plen, granted, status);
}

void __stdcall start(void) {
    char marker[16];
    int native = GetEnvironmentVariableA("OSYX", marker, sizeof marker) > 0;
    DWORD size = sizeof world;
    CreateWellKnownSid(WinWorldSid, NULL, world, &size);
    size = sizeof users;
    CreateWellKnownSid(WinBuiltinUsersSid, NULL, users, &size);
    size = sizeof admins;
    CreateWellKnownSid(WinBuiltinAdministratorsSid, NULL, admins, &size);

    /* ---- string SIDs and heap SIDs ---- */
    PSID s = NULL;
    check("string SID converts", ConvertStringSidToSidW(L"S-1-5-32-544", &s) && EqualSid(s, admins));
    LocalFree(s);
    s = NULL;
    check("SDDL alias converts", ConvertStringSidToSidW(L"WD", &s) && EqualSid(s, world));
    LocalFree(s);
    check("malformed string SID rejected", !ConvertStringSidToSidW(L"S-1-x-3", &s) && GetLastError() == ERROR_INVALID_SID);
    LPWSTR text = NULL;
    check("SID converts back to its string", ConvertSidToStringSidW(users, &text) && wcs_eq(text, L"S-1-5-32-545"));
    LocalFree(text);
    SID_IDENTIFIER_AUTHORITY nt = SECURITY_NT_AUTHORITY;
    PSID heap = NULL;
    BOOL made = AllocateAndInitializeSid(&nt, 2, SECURITY_BUILTIN_DOMAIN_RID, DOMAIN_ALIAS_RID_USERS, 0, 0, 0, 0, 0, 0, &heap);
    check("AllocateAndInitializeSid builds Users", made && EqualSid(heap, users));
    check("FreeSid returns NULL", FreeSid(heap) == NULL);

    /* ---- trustees, SetEntriesInAcl, MapGenericMask, AddMandatoryAce ---- */
    TRUSTEE_W t;
    memset(&t, 0xcc, sizeof t);
    BuildTrusteeWithSidW(&t, world);
    check("BuildTrusteeWithSidW fills a SID trustee",
          t.pMultipleTrustee == NULL && t.MultipleTrusteeOperation == NO_MULTIPLE_TRUSTEE && t.TrusteeForm == TRUSTEE_IS_SID &&
              t.TrusteeType == TRUSTEE_IS_UNKNOWN && t.ptstrName == (LPWSTR)world);
    EXPLICIT_ACCESS_W ea[2];
    memset(ea, 0, sizeof ea);
    ea[0].grfAccessPermissions = GENERIC_READ;
    ea[0].grfAccessMode = GRANT_ACCESS;
    BuildTrusteeWithSidW(&ea[0].Trustee, world);
    ea[1].grfAccessPermissions = GENERIC_WRITE;
    ea[1].grfAccessMode = DENY_ACCESS;
    BuildTrusteeWithSidW(&ea[1].Trustee, users);
    PACL acl = NULL;
    DWORD r = SetEntriesInAclW(2, ea, NULL, &acl);
    ACCESS_ALLOWED_ACE *a0 = NULL, *a1 = NULL;
    check("SetEntriesInAclW builds a new ACL", r == ERROR_SUCCESS && acl && acl->AceCount == 2 && GetAce(acl, 0, (void **)&a0) && GetAce(acl, 1, (void **)&a1));
    check("deny ACEs come first", a0 && a0->Header.AceType == ACCESS_DENIED_ACE_TYPE && EqualSid(&a0->SidStart, users) && a0->Mask == GENERIC_WRITE);
    check("granted ACE keeps generic rights", a1 && a1->Header.AceType == ACCESS_ALLOWED_ACE_TYPE && EqualSid(&a1->SidStart, world) && a1->Mask == GENERIC_READ);
    EXPLICIT_ACCESS_W set;
    memset(&set, 0, sizeof set);
    set.grfAccessPermissions = GENERIC_ALL;
    set.grfAccessMode = SET_ACCESS;
    BuildTrusteeWithSidW(&set.Trustee, world);
    PACL acl2 = NULL;
    r = SetEntriesInAclW(1, &set, acl, &acl2);
    ACCESS_ALLOWED_ACE *b0 = NULL, *b1 = NULL;
    check("SET_ACCESS replaces the trustee's ACEs",
          r == ERROR_SUCCESS && acl2 && acl2->AceCount == 2 && GetAce(acl2, 0, (void **)&b0) && GetAce(acl2, 1, (void **)&b1) &&
              b0->Header.AceType == ACCESS_DENIED_ACE_TYPE && b1->Header.AceType == ACCESS_ALLOWED_ACE_TYPE && b1->Mask == GENERIC_ALL &&
              EqualSid(&b1->SidStart, world));
    EXPLICIT_ACCESS_W revoke;
    memset(&revoke, 0, sizeof revoke);
    revoke.grfAccessMode = REVOKE_ACCESS;
    BuildTrusteeWithSidW(&revoke.Trustee, world);
    PACL acl3 = NULL;
    r = SetEntriesInAclW(1, &revoke, acl2, &acl3);
    check("REVOKE_ACCESS removes the trustee's grants", r == ERROR_SUCCESS && acl3 && acl3->AceCount == 1);
    LocalFree(acl);
    LocalFree(acl2);
    LocalFree(acl3);
    DWORD mask = GENERIC_READ | DELETE;
    MapGenericMask(&mask, &file_map);
    check("MapGenericMask maps generic rights", mask == (FILE_GENERIC_READ | DELETE));
    BYTE label_acl[64], low[SECURITY_MAX_SID_SIZE];
    size = sizeof low;
    CreateWellKnownSid(WinLowLabelSid, NULL, low, &size);
    InitializeAcl((PACL)label_acl, sizeof label_acl, ACL_REVISION);
    BOOL labeled = AddMandatoryAce((PACL)label_acl, ACL_REVISION, 0, SYSTEM_MANDATORY_LABEL_NO_WRITE_UP, low);
    SYSTEM_MANDATORY_LABEL_ACE *lace = NULL;
    check("AddMandatoryAce adds a label ACE",
          labeled && GetAce((PACL)label_acl, 0, (void **)&lace) && lace->Header.AceType == SYSTEM_MANDATORY_LABEL_ACE_TYPE &&
              lace->Mask == SYSTEM_MANDATORY_LABEL_NO_WRITE_UP && EqualSid(&lace->SidStart, low));
    check("AddMandatoryAce rejects a non-label SID", !AddMandatoryAce((PACL)label_acl, ACL_REVISION, 0, 1, world));

    /* ---- kernel object security ---- */
    HANDLE process = NULL;
    OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ADJUST_PRIVILEGES | TOKEN_IMPERSONATE, &process);
    PSID me = token_user(process);
    HANDLE unnamed = CreateEventW(NULL, TRUE, FALSE, NULL);
    PSID owner = (PSID)1;
    PACL dacl = (PACL)1;
    PSECURITY_DESCRIPTOR psd = NULL;
    r = GetSecurityInfo(unnamed, SE_KERNEL_OBJECT, OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION, &owner, NULL, &dacl, NULL, &psd);
    check("an unnamed event has no security", r == ERROR_SUCCESS && owner == NULL && dacl == NULL);
    LocalFree(psd);
    check("so its security cannot be set", SetSecurityInfo(unnamed, SE_KERNEL_OBJECT, DACL_SECURITY_INFORMATION, NULL, NULL, NULL, NULL) == ERROR_NO_SECURITY_ON_OBJECT);
    CloseHandle(unnamed);
    HANDLE ev = CreateEventW(NULL, TRUE, FALSE, L"osyx-sandbox-event");
    owner = NULL;
    dacl = NULL;
    psd = NULL;
    r = GetSecurityInfo(ev, SE_KERNEL_OBJECT, OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION, &owner, NULL, &dacl, NULL, &psd);
    check("GetSecurityInfo reads a named event's descriptor", r == ERROR_SUCCESS && psd && owner && IsValidSid(owner) && dacl && IsValidAcl(dacl));
    SECURITY_DESCRIPTOR_CONTROL control = 0;
    DWORD revision = 0;
    check("returned descriptor is self-relative", psd && GetSecurityDescriptorControl(psd, &control, &revision) && (control & SE_SELF_RELATIVE) && (control & SE_DACL_PRESENT));
    LocalFree(psd);
    BYTE one[64];
    InitializeAcl((PACL)one, sizeof one, ACL_REVISION);
    AddAccessAllowedAce((PACL)one, ACL_REVISION, GENERIC_ALL, world);
    r = SetSecurityInfo(ev, SE_KERNEL_OBJECT, DACL_SECURITY_INFORMATION, NULL, NULL, (PACL)one, NULL);
    psd = NULL;
    dacl = NULL;
    ACCESS_ALLOWED_ACE *e0 = NULL;
    DWORD r2 = GetSecurityInfo(ev, SE_KERNEL_OBJECT, DACL_SECURITY_INFORMATION, NULL, NULL, &dacl, NULL, &psd);
    check("SetSecurityInfo replaces the DACL", r == ERROR_SUCCESS && r2 == ERROR_SUCCESS && dacl && dacl->AceCount == 1 && GetAce(dacl, 0, (void **)&e0) && EqualSid(&e0->SidStart, world));
    check("stored ACE rights are mapped to the event's", e0 && e0->Mask == EVENT_ALL_ACCESS);
    LocalFree(psd);
    r = SetSecurityInfo(ev, SE_KERNEL_OBJECT, LABEL_SECURITY_INFORMATION, NULL, NULL, NULL, (PACL)label_acl);
    PACL sacl = NULL;
    psd = NULL;
    r2 = GetSecurityInfo(ev, SE_KERNEL_OBJECT, LABEL_SECURITY_INFORMATION, NULL, NULL, NULL, &sacl, &psd);
    SYSTEM_MANDATORY_LABEL_ACE *l0 = NULL;
    check("an object can be labelled Low", r == ERROR_SUCCESS && r2 == ERROR_SUCCESS && sacl && sacl->AceCount == 1 && GetAce(sacl, 0, (void **)&l0) && EqualSid(&l0->SidStart, low));
    LocalFree(psd);
    SECURITY_DESCRIPTOR nulldacl;
    InitializeSecurityDescriptor(&nulldacl, SECURITY_DESCRIPTOR_REVISION);
    SetSecurityDescriptorDacl(&nulldacl, TRUE, NULL, FALSE);
    BOOL set_ok = SetKernelObjectSecurity(ev, DACL_SECURITY_INFORMATION, &nulldacl);
    psd = NULL;
    dacl = (PACL)1;
    r2 = GetSecurityInfo(ev, SE_KERNEL_OBJECT, DACL_SECURITY_INFORMATION, NULL, NULL, &dacl, NULL, &psd);
    check("SetKernelObjectSecurity sets a NULL DACL", set_ok && r2 == ERROR_SUCCESS && dacl == NULL);
    LocalFree(psd);
    check("SetSecurityInfo rejects a foreign owner", SetSecurityInfo(ev, SE_KERNEL_OBJECT, OWNER_SECURITY_INFORMATION, world, NULL, NULL, NULL) != ERROR_SUCCESS);
    CloseHandle(ev);
    check("GetSecurityInfo rejects a closed handle", GetSecurityInfo(ev, SE_KERNEL_OBJECT, DACL_SECURITY_INFORMATION, NULL, NULL, &dacl, NULL, &psd) != ERROR_SUCCESS);

    /* ---- file security ---- */
    WCHAR dir[MAX_PATH], file[MAX_PATH];
    GetTempPathW(MAX_PATH, dir);
    GetTempFileNameW(dir, L"sbx", 0, file);
    owner = NULL;
    dacl = NULL;
    psd = NULL;
    r = GetNamedSecurityInfoW(file, SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION, &owner, NULL, &dacl, NULL, &psd);
    check("GetNamedSecurityInfoW reads a file's descriptor", r == ERROR_SUCCESS && owner && IsValidSid(owner) && dacl && IsValidAcl(dacl));
    if (native) check("an OSyx file is owned by its OSyx user", owner && me && EqualSid(owner, me));
    LocalFree(psd);
    if (native) {
        BYTE fa[96];
        InitializeAcl((PACL)fa, sizeof fa, ACL_REVISION);
        AddAccessAllowedAce((PACL)fa, ACL_REVISION, GENERIC_READ | GENERIC_WRITE, me);
        r = SetNamedSecurityInfoW(file, SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, NULL, NULL, (PACL)fa, NULL);
        psd = NULL;
        dacl = NULL;
        GetNamedSecurityInfoW(file, SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, NULL, NULL, &dacl, NULL, &psd);
        DWORD world_mask = 0xffffffff, user_mask = 0;
        for (DWORD i = 0; dacl && i < dacl->AceCount; i++) {
            ACCESS_ALLOWED_ACE *x;
            GetAce(dacl, i, (void **)&x);
            if (EqualSid(&x->SidStart, world)) world_mask = x->Mask;
            if (EqualSid(&x->SidStart, me)) user_mask = x->Mask;
        }
        check("a file DACL becomes the file's mode", r == ERROR_SUCCESS && world_mask == 0 && (user_mask & FILE_READ_DATA) && (user_mask & FILE_WRITE_DATA) && !(user_mask & FILE_EXECUTE));
        LocalFree(psd);
        InitializeAcl((PACL)fa, sizeof fa, ACL_REVISION);
        AddAccessDeniedAce((PACL)fa, ACL_REVISION, GENERIC_WRITE, world);
        check("a DACL the mode cannot express is refused", SetNamedSecurityInfoW(file, SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, NULL, NULL, (PACL)fa, NULL) == ERROR_NOT_SUPPORTED);
    }
    DeleteFileW(file);
    check("missing file reported", GetNamedSecurityInfoW(file, SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION, &owner, NULL, NULL, NULL, &psd) == ERROR_FILE_NOT_FOUND);

    /* ---- AccessCheck ---- */
    HANDLE imp = NULL;
    check("DuplicateTokenEx makes an impersonation token", DuplicateTokenEx(process, TOKEN_QUERY | TOKEN_IMPERSONATE | TOKEN_ADJUST_DEFAULT | TOKEN_DUPLICATE, NULL, SecurityImpersonation, TokenImpersonation, &imp));
    TOKEN_TYPE type = 0;
    DWORD n = 0;
    check("the duplicate is an impersonation token", GetTokenInformation(imp, TokenType, &type, sizeof type, &n) && type == TokenImpersonation);
    BYTE ab[128];
    InitializeAcl((PACL)ab, sizeof ab, ACL_REVISION);
    AddAccessAllowedAce((PACL)ab, ACL_REVISION, FILE_GENERIC_READ, me);
    SECURITY_DESCRIPTOR sd;
    make_sd(&sd, me, (PACL)ab, TRUE);
    DWORD granted;
    BOOL status;
    check("AccessCheck grants what the DACL allows", check_access(imp, &sd, FILE_READ_DATA, &granted, &status) && status && granted == FILE_READ_DATA);
    check("AccessCheck refuses what it does not", check_access(imp, &sd, FILE_WRITE_DATA, &granted, &status) && !status && granted == 0 && GetLastError() == ERROR_ACCESS_DENIED);
    check("the owner may always read the descriptor", check_access(imp, &sd, READ_CONTROL | WRITE_DAC, &granted, &status) && status);
    check("MAXIMUM_ALLOWED reports the granted rights", check_access(imp, &sd, MAXIMUM_ALLOWED, &granted, &status) && status && (granted & FILE_GENERIC_READ) == FILE_GENERIC_READ && !(granted & FILE_WRITE_DATA));
    InitializeAcl((PACL)ab, sizeof ab, ACL_REVISION);
    AddAccessDeniedAce((PACL)ab, ACL_REVISION, FILE_READ_DATA, world);
    AddAccessAllowedAce((PACL)ab, ACL_REVISION, FILE_ALL_ACCESS, me);
    make_sd(&sd, world, (PACL)ab, TRUE);
    check("a deny ACE before the grant wins", check_access(imp, &sd, FILE_READ_DATA, &granted, &status) && !status);
    check("rights the deny ACE leaves are granted", check_access(imp, &sd, FILE_WRITE_DATA, &granted, &status) && status);
    make_sd(&sd, world, NULL, TRUE);
    check("a NULL DACL grants everything", check_access(imp, &sd, FILE_ALL_ACCESS, &granted, &status) && status && granted == FILE_ALL_ACCESS);
    check("AccessCheck needs mapped rights", !check_access(imp, &sd, GENERIC_READ, &granted, &status) && GetLastError() == ERROR_GENERIC_NOT_MAPPED);
    check("AccessCheck needs an impersonation token", !check_access(process, &sd, FILE_READ_DATA, &granted, &status) && GetLastError() == ERROR_NO_IMPERSONATION_TOKEN);

    /* Restricted token: the restricting SIDs must grant access too. */
    SID_AND_ATTRIBUTES restrict_to = {world, 0};
    HANDLE restricted = NULL, rimp = NULL;
    check("CreateRestrictedToken", CreateRestrictedToken(process, DISABLE_MAX_PRIVILEGE, 0, NULL, 0, NULL, 1, &restrict_to, &restricted));
    check("restricted token duplicates for impersonation", DuplicateTokenEx(restricted, TOKEN_QUERY | TOKEN_IMPERSONATE, NULL, SecurityImpersonation, TokenImpersonation, &rimp));
    InitializeAcl((PACL)ab, sizeof ab, ACL_REVISION);
    AddAccessAllowedAce((PACL)ab, ACL_REVISION, FILE_GENERIC_READ, me);
    make_sd(&sd, admins, (PACL)ab, TRUE);
    check("a grant to the user alone is not enough", check_access(rimp, &sd, FILE_READ_DATA, &granted, &status) && !status);
    AddAccessAllowedAce((PACL)ab, ACL_REVISION, FILE_GENERIC_READ, world);
    check("a grant to the restricting SID as well is", check_access(rimp, &sd, FILE_READ_DATA, &granted, &status) && status);
    BYTE rs[256];
    check("TokenRestrictedSids lists the restriction", GetTokenInformation(restricted, TokenRestrictedSids, rs, sizeof rs, &n) && ((TOKEN_GROUPS *)rs)->GroupCount == 1 && EqualSid(((TOKEN_GROUPS *)rs)->Groups[0].Sid, world));
    BYTE pv[1024];
    check("DISABLE_MAX_PRIVILEGE keeps only SeChangeNotify", GetTokenInformation(restricted, TokenPrivileges, pv, sizeof pv, &n) && ((TOKEN_PRIVILEGES *)pv)->PrivilegeCount == 1);

    /* Integrity: a Low token cannot write up to a Medium (unlabelled) object. */
    TOKEN_MANDATORY_LABEL tml = {{low, SE_GROUP_INTEGRITY}};
    check("an impersonation token can be lowered to Low", SetTokenInformation(imp, TokenIntegrityLevel, &tml, sizeof tml + GetLengthSid(low)));
    InitializeAcl((PACL)ab, sizeof ab, ACL_REVISION);
    AddAccessAllowedAce((PACL)ab, ACL_REVISION, FILE_ALL_ACCESS, world);
    make_sd(&sd, admins, (PACL)ab, TRUE);
    check("Low integrity cannot write up", check_access(imp, &sd, FILE_WRITE_DATA, &granted, &status) && !status);
    check("Low integrity can read down", check_access(imp, &sd, FILE_READ_DATA, &granted, &status) && status);

    /* ---- privileges and thread tokens ---- */
    TOKEN_PRIVILEGES tp = {1, {{{0, 0}, SE_PRIVILEGE_ENABLED}}};
    LookupPrivilegeValueW(NULL, L"SeChangeNotifyPrivilege", &tp.Privileges[0].Luid);
    SetLastError(0xdead);
    check("AdjustTokenPrivileges enables a held privilege", AdjustTokenPrivileges(process, FALSE, &tp, 0, NULL, NULL) && GetLastError() == ERROR_SUCCESS);
    LookupPrivilegeValueW(NULL, L"SeTcbPrivilege", &tp.Privileges[0].Luid);
    check("a privilege not held is reported", AdjustTokenPrivileges(process, FALSE, &tp, 0, NULL, NULL) && GetLastError() == ERROR_NOT_ALL_ASSIGNED);
    HANDLE same = NULL, got = NULL;
    DuplicateTokenEx(process, TOKEN_QUERY | TOKEN_IMPERSONATE, NULL, SecurityImpersonation, TokenImpersonation, &same);
    check("SetThreadToken impersonates", SetThreadToken(NULL, same));
    check("OpenThreadToken returns it", OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, TRUE, &got) && GetTokenInformation(got, TokenType, &type, sizeof type, &n) && type == TokenImpersonation);
    CloseHandle(got);
    check("RevertToSelf", RevertToSelf());
    check("no thread token after reverting", !OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, TRUE, &got) && GetLastError() == ERROR_NO_TOKEN);
    check("SetThreadToken refuses a primary token", !SetThreadToken(NULL, process));
    if (native) {
        check("a restricted token cannot run on OSyx", !SetThreadToken(NULL, rimp) && GetLastError() == ERROR_NOT_SUPPORTED);
        STARTUPINFOW si = {sizeof si};
        PROCESS_INFORMATION pi;
        WCHAR cmd[] = L"C:\\opt\\wintests\\minimal.exe";
        HANDLE rprimary = NULL;
        DuplicateTokenEx(restricted, TOKEN_ALL_ACCESS, NULL, SecurityImpersonation, TokenPrimary, &rprimary);
        check("no process starts with a restricted token", !CreateProcessAsUserW(rprimary, NULL, cmd, NULL, NULL, FALSE, 0, NULL, NULL, &si, &pi) && GetLastError() == ERROR_NOT_SUPPORTED);
        CloseHandle(rprimary);
    }
    CloseHandle(same);
    CloseHandle(rimp);
    CloseHandle(restricted);
    CloseHandle(imp);
    CloseHandle(process);

    /* ---- event log and registry ---- */
    HANDLE src = RegisterEventSourceW(NULL, L"OSyx sandbox test");
    LPCWSTR strings[1] = {L"sandbox test event"};
    check("RegisterEventSourceW", src != NULL);
    check("ReportEventW", src && ReportEventW(src, EVENTLOG_INFORMATION_TYPE, 0, 1, NULL, 1, 0, strings, NULL));
    check("DeregisterEventSource", src && DeregisterEventSource(src));
    check("RegDisablePredefinedCache", RegDisablePredefinedCache() == ERROR_SUCCESS);
    finish("sandbox");
}
