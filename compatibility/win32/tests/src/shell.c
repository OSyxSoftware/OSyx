/* shell32 known folders and the ole32 task allocator. The profile
 * folders lie below the profile, CSIDL and FOLDERID lookups agree, a
 * missing folder is reported unless created or not verified (checked on
 * OSyx, where a fresh profile has no Videos folder). The same PE runs on
 * Windows. */
#include "common.h"
#include <shlobj.h>

_Static_assert(sizeof(WCHAR) == 2 && sizeof(HRESULT) == 4, "Windows string and HRESULT ABI");
_Static_assert(sizeof(GUID) == 16 && sizeof(void *) == 8, "Windows x64 shell ABI");
_Static_assert(sizeof(STARTUPINFOW) == 104 && sizeof(PROCESS_INFORMATION) == 24, "Windows child process ABI");

typedef HRESULT (WINAPI *GetAppID)(PWSTR *);
typedef HRESULT (WINAPI *SetAppID)(PCWSTR);
static GetAppID get_app_id;
static SetAppID set_app_id;

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
    const WCHAR *start = prefix;
    while (*prefix && *s == *prefix) s++, prefix++;
    /* A root profile ("C:\") already ends in the separator. */
    return *prefix == 0 && (*s == L'\\' || (prefix > start && prefix[-1] == L'\\'));
}

static int is_dir(const WCHAR *p) {
    DWORD a = GetFileAttributesW(p);
    return a != INVALID_FILE_ATTRIBUTES && (a & FILE_ATTRIBUTE_DIRECTORY);
}

static DWORD WINAPI app_id_thread(void *unused) {
    (void)unused;
    WCHAR *id=NULL;
    if (get_app_id(&id)!=S_OK || !id || !wide_eq(id,L"OSyx.Shell.Parent")) return 1;
    CoTaskMemFree(id);
    return set_app_id(L"OSyx.Shell.Worker")==S_OK ? 0 : 2;
}

static void app_ids(void) {
    WCHAR *a=(WCHAR *)1,*b=NULL;
    SetLastError(777);
    check_eq("no explicit process AppUserModelID",(DWORD)get_app_id(&a),(DWORD)E_FAIL);
    check("missing AppUserModelID clears output",a==NULL);
    check_eq("AppUserModelID query preserves last error",GetLastError(),777);
    WCHAR name[]=L"OSyx.Shell.Case\u017b\U0001f680";
    check_eq("SetCurrentProcessExplicitAppUserModelID",set_app_id(name),S_OK);
    name[0]='X';
    check("setter copies UTF-16 input",get_app_id(&a)==S_OK && a && wide_eq(a,L"OSyx.Shell.Case\u017b\U0001f680"));
    check("each AppUserModelID query allocates independently",get_app_id(&b)==S_OK && b && a!=b && wide_eq(a,b));
    if (a) a[0]='Y';
    check("returned AppUserModelID does not alias another result",b && b[0]=='O');
    CoTaskMemFree(a); CoTaskMemFree(b); a=NULL; b=NULL;
    WCHAR limit[130]; for(int i=0;i<129;i++)limit[i]='a'; limit[129]=0;
    limit[127]=0;
    check_eq("AppUserModelID accepts 127 UTF-16 units",set_app_id(limit),S_OK);
    limit[127]='a'; limit[128]=0;
    check_eq("AppUserModelID rejects 128 UTF-16 units",(DWORD)set_app_id(limit),(DWORD)E_INVALIDARG);
    check("oversized AppUserModelID preserves previous value",get_app_id(&a)==S_OK && a && wide_len(a)==127);
    CoTaskMemFree(a); a=NULL;
    check_eq("empty AppUserModelID clears identity",set_app_id(L""),S_OK);
    a=(WCHAR *)1;
    check("empty ID is absent on retrieval",get_app_id(&a)==E_FAIL && a==NULL);
    check_eq("AppUserModelID accepts spaces verbatim",set_app_id(L"test with spaces"),S_OK);
    check("space-containing AppUserModelID round trip",get_app_id(&a)==S_OK && a && wide_eq(a,L"test with spaces"));
    CoTaskMemFree(a); a=NULL;
    check_eq("NULL AppUserModelID returns E_INVALIDARG",(DWORD)set_app_id(NULL),(DWORD)E_INVALIDARG);
    a=(WCHAR *)1;
    check("NULL setter clears old identity despite failure",get_app_id(&a)==E_FAIL && a==NULL);
    check_eq("set parent AppUserModelID",set_app_id(L"OSyx.Shell.Parent"),S_OK);
    HANDLE thread=CreateThread(NULL,0,app_id_thread,NULL,0,NULL); DWORD code=99;
    check("process AppUserModelID is shared across threads",thread && WaitForSingleObject(thread,5000)==WAIT_OBJECT_0 && GetExitCodeThread(thread,&code) && code==0);
    if(thread)CloseHandle(thread);
    check("worker updates process AppUserModelID",get_app_id(&a)==S_OK && a && wide_eq(a,L"OSyx.Shell.Worker"));
    CoTaskMemFree(a); a=NULL;
    WCHAR exe[MAX_PATH],cmd[MAX_PATH+32];
    GetModuleFileNameW(NULL,exe,MAX_PATH);
    DWORD n=0;cmd[n++]='"';for(DWORD i=0;exe[i];i++)cmd[n++]=exe[i];cmd[n++]='"';
    static const WCHAR tail[]=L" --appid-child";for(DWORD i=0;i<sizeof(tail)/sizeof(tail[0]);i++)cmd[n++]=tail[i];
    STARTUPINFOW si={0};si.cb=sizeof si; PROCESS_INFORMATION pi={0};
    BOOL child=CreateProcessW(exe,cmd,NULL,NULL,FALSE,0,NULL,NULL,&si,&pi);code=99;
    check("explicit AppUserModelID is process-local",child && WaitForSingleObject(pi.hProcess,10000)==WAIT_OBJECT_0 && GetExitCodeProcess(pi.hProcess,&code) && code==0);
    if(child){CloseHandle(pi.hThread);CloseHandle(pi.hProcess);}
    check("child does not alter parent's AppUserModelID",get_app_id(&a)==S_OK && a && wide_eq(a,L"OSyx.Shell.Worker"));
    CoTaskMemFree(a);
    check_eq("clear explicit AppUserModelID",set_app_id(L""),S_OK);
}

void __stdcall start(void) {
    HMODULE sh=LoadLibraryA("shell32.dll");
    get_app_id=(GetAppID)GetProcAddress(sh,"GetCurrentProcessExplicitAppUserModelID");
    set_app_id=(SetAppID)GetProcAddress(sh,"SetCurrentProcessExplicitAppUserModelID");
    check("process AppUserModelID exports",get_app_id && set_app_id);
    if (!get_app_id || !set_app_id) finish("shell");
    if (wide_ends(GetCommandLineW(),L" --appid-child")) {
        WCHAR *id=(WCHAR *)1;
        check("child starts without an explicit AppUserModelID",get_app_id(&id)==E_FAIL && id==NULL);
        check("child can set its own AppUserModelID",set_app_id(L"OSyx.Shell.Child")==S_OK);
        finish("shell-child");
    }
    app_ids();
    char marker[8];
    int native = GetEnvironmentVariableA("OSYX", marker, sizeof marker) > 0;

    WCHAR *profile = NULL, *roaming = NULL, *local = NULL, *sys = NULL;
    check("profile folder", SHGetKnownFolderPath(&ID_Profile, 0, NULL, &profile) == S_OK && profile && profile[1] == L':' && is_dir(profile));
    check("roaming AppData, created", SHGetKnownFolderPath(&ID_Roaming, KF_FLAG_CREATE, NULL, &roaming) == S_OK && roaming &&
                                          wide_ends(roaming, L"\\AppData\\Roaming") && is_dir(roaming));
    check("local AppData, created", SHGetKnownFolderPath(&ID_Local, KF_FLAG_CREATE, NULL, &local) == S_OK && local &&
                                        wide_ends(local, L"\\AppData\\Local") && is_dir(local));
    check("AppData lies in the profile", profile && roaming && local && starts_with(roaming, profile) && starts_with(local, profile));
    /* Gecko asks with DONT_VERIFY | NO_ALIAS | SIMPLE_IDLIST. */
    WCHAR *again = NULL;
    check("path flags that only shape ID lists", SHGetKnownFolderPath(&ID_Roaming, 0x5100, NULL, &again) == S_OK && again && roaming && wide_eq(again, roaming));
    CoTaskMemFree(again);

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
