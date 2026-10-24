/* shlwapi path and string helpers, IsOS; also runs on Windows. */
#include "common.h"
#include <shlwapi.h>

#define BS L"\\"

static int wide_eq(const WCHAR *a, const WCHAR *b) { while(*a && *a==*b){a++;b++;} return *a==*b; }

static void combine(const char *what, const WCHAR *dir, const WCHAR *file, const WCHAR *want) {
    WCHAR d[MAX_PATH]; d[0]=L'?';
    LPWSTR r=PathCombineW(d,dir,file);
    check(what,want ? (r==d && wide_eq(d,want)) : (r==NULL && d[0]==0));
}

static void paths(void) {
    combine("relative file",L"C:" BS L"a",L"b" BS L"c.txt",L"C:" BS L"a" BS L"b" BS L"c.txt");
    combine("parent element",L"C:" BS L"a" BS,L".." BS L"c.txt",L"C:" BS L"c.txt");
    combine("absolute file wins",L"C:" BS L"a",L"D:" BS L"q",L"D:" BS L"q");
    combine("rooted file keeps the drive",L"C:" BS L"a",BS L"q",L"C:" BS L"q");
    combine("no directory",NULL,L"q" BS L"y",L"q" BS L"y");
    combine("no file",L"C:" BS L"a",NULL,L"C:" BS L"a");
    combine("neither",NULL,NULL,NULL);
    combine("empty file",L"C:" BS L"a",L"",L"C:" BS L"a");
    combine("empty directory",L"",L"q",L"q");
    combine("relative directory",L"a",L"b",L"a" BS L"b");
    combine("dot elements",L"C:" BS L"a" BS L"." BS L"b",L".",L"C:" BS L"a" BS L"b");
    combine("trailing backslash kept",L"C:" BS L"a",L"b" BS,L"C:" BS L"a" BS L"b" BS);

    const WCHAR *n1=L"C:" BS L"a" BS L"b.txt", *n2=L"C:" BS L"a" BS L"b" BS, *n3=L"C:" BS, *n4=BS BS L"srv" BS L"share";
    check("file name",wide_eq(PathFindFileNameW(n1),L"b.txt"));
    check("folder with trailing backslash",wide_eq(PathFindFileNameW(n2),L"b" BS));
    check("drive root",PathFindFileNameW(n3)==n3);
    check("UNC share",wide_eq(PathFindFileNameW(n4),L"share"));
    check("forward slash",wide_eq(PathFindFileNameW(L"a/b"),L"b"));
    check("drive-relative",wide_eq(PathFindFileNameW(L"C:q"),L"q"));
    check("doubled backslash",wide_eq(PathFindFileNameW(L"C:" BS L"a" BS BS L"b"),L"b"));

    check("UNC is a network path",PathIsNetworkPathW(BS BS L"srv" BS L"s"));
    check("drive path is local",!PathIsNetworkPathW(L"C:" BS L"q"));
    check("rooted path is local",!PathIsNetworkPathW(BS L"q"));
}

static void unexpand(void) {
    WCHAR p[MAX_PATH], o[MAX_PATH];
    DWORD n=GetEnvironmentVariableW(L"APPDATA",p,MAX_PATH);
    if(n && n<MAX_PATH-8) {
        lstrcatW(p,BS L"Zen");
        check("APPDATA folder",PathUnExpandEnvStringsW(p,o,MAX_PATH) && wide_eq(o,L"%APPDATA%" BS L"Zen"));
        check("result must fit",!PathUnExpandEnvStringsW(p,o,5));
    } else check("APPDATA is set",0);
    n=GetEnvironmentVariableW(L"USERPROFILE",p,MAX_PATH);
    if(n && n<MAX_PATH-8) {
        check("USERPROFILE itself",PathUnExpandEnvStringsW(p,o,MAX_PATH) && wide_eq(o,L"%USERPROFILE%"));
        lstrcatW(p,BS L"q");
        check("USERPROFILE folder",PathUnExpandEnvStringsW(p,o,MAX_PATH) && wide_eq(o,L"%USERPROFILE%" BS L"q"));
    } else check("USERPROFILE is set",0);
    check("unrelated path",!PathUnExpandEnvStringsW(L"Q:" BS L"nothing",o,MAX_PATH));
}

static void strings(void) {
    const WCHAR *h=L"Hello World";
    check("StrStrIW ignores case",StrStrIW(h,L"WORLD")==h+6);
    check("StrStrIW at the start",StrStrIW(h,L"hello")==h);
    check("StrStrIW misses",StrStrIW(h,L"xyz")==NULL);
    check("StrStrIW empty needle",StrStrIW(h,L"")==NULL);
    check("StrStrIW longer needle",StrStrIW(L"ab",L"abc")==NULL);
}

typedef BOOL (WINAPI *isos_t)(DWORD);

static void is_os(void) {
    isos_t f=(isos_t)GetProcAddress(LoadLibraryW(L"shlwapi.dll"),(LPCSTR)437);
    check("IsOS by ordinal",f!=NULL);
    if(!f) return;
    check("OS_NT",f(1));
    check("not Windows 9x",!f(0));
    check("OS_WIN2000ORGREATER",f(7));
    check("OS_XPORGREATER",f(18));
    check("OS_PROFESSIONAL",f(20));
    check("not OS_ANYSERVER",!f(29));
    check("not OS_WOW6432",!f(30));
    check("not OS_DOMAINMEMBER",!f(28));
    check("not OS_TABLETPC",!f(33));
}

void __stdcall start(void) {
    paths();
    unexpand();
    strings();
    is_os();
    finish("shlpath");
}
