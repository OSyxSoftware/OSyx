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

static int str_eq(const char *a, const char *b) { while(*a && *a==*b){a++;b++;} return *a==*b; }

/* The second set of path helpers (shlstr.rs). */
static void more_paths(void) {
    WCHAR p[MAX_PATH], t[MAX_PATH];
    check("relative path",PathIsRelativeW(L"a" BS L"b") && PathIsRelativeW(L""));
    check("rooted, drive and drive-relative paths are not relative",
          !PathIsRelativeW(BS L"a") && !PathIsRelativeW(L"C:" BS L"a") && !PathIsRelativeW(L"C:a"));
    check("roots",PathIsRootW(L"C:" BS) && PathIsRootW(BS) && PathIsRootW(BS BS L"srv" BS L"share") && PathIsRootW(BS BS L"srv"));
    check("not roots",!PathIsRootW(L"C:") && !PathIsRootW(L"C:" BS L"a") && !PathIsRootW(BS BS L"srv" BS L"share" BS L"x") && !PathIsRootW(NULL));
    check("UNC",PathIsUNCW(BS BS L"srv" BS L"s") && !PathIsUNCW(L"C:" BS L"x") && !PathIsUNCW(BS L"x"));
    check("file specification",PathIsFileSpecW(L"a.txt") && !PathIsFileSpecW(L"a" BS L"b") && !PathIsFileSpecW(L"C:a"));
    check("URLs",PathIsURLW(L"http://osyx.example/") && PathIsURLW(L"mailto:a@b") && !PathIsURLW(L"C:" BS L"x") && !PathIsURLW(L"x"));
    check("prefix on element boundaries",PathIsPrefixW(L"C:" BS L"a",L"C:" BS L"a" BS L"b") && !PathIsPrefixW(L"C:" BS L"a",L"C:" BS L"ab"));

    DWORD n=GetTempPathW(MAX_PATH,t);
    if(n && n<MAX_PATH-16) {
        check("the temporary folder is a directory",PathIsDirectoryW(t)!=0);
        lstrcpyW(p,t); lstrcatW(p,L"osyx-shlpath.tmp");
        HANDLE f=CreateFileW(p,GENERIC_WRITE,0,NULL,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,NULL);
        if(f!=INVALID_HANDLE_VALUE) {
            CloseHandle(f);
            check("a file is not a directory",!PathIsDirectoryW(p) && PathFileExistsW(p));
            char a[MAX_PATH]; int i=0; for(;p[i] && i<MAX_PATH-1;i++) a[i]=(char)p[i]; a[i]=0;
            check("PathFileExistsA and PathIsDirectoryA",PathFileExistsA(a) && !PathIsDirectoryA(a));
            DeleteFileW(p);
            check("a deleted file does not exist",!PathFileExistsW(p) && !PathIsDirectoryW(p) && !PathFileExistsA(a));
        } else check("create a file in the temporary folder",0);
    } else check("GetTempPathW",0);

    lstrcpyW(p,L"C:" BS L"a");
    LPWSTR end=PathAddBackslashW(p);
    check("PathAddBackslashW",wide_eq(p,L"C:" BS L"a" BS) && end==p+5 && *end==0);
    check("PathAddBackslashW keeps one",PathAddBackslashW(p)==p+5 && wide_eq(p,L"C:" BS L"a" BS));
    PathRemoveBackslashW(p);
    check("PathRemoveBackslashW",wide_eq(p,L"C:" BS L"a"));
    lstrcpyW(p,L"C:" BS); PathRemoveBackslashW(p);
    check("PathRemoveBackslashW keeps a root",wide_eq(p,L"C:" BS));

    lstrcpyW(p,L"C:" BS L"a" BS L"b.txt"); PathStripPathW(p);
    check("PathStripPathW",wide_eq(p,L"b.txt"));
    lstrcpyW(p,L"C:" BS L"a" BS L"b");
    check("PathStripToRootW drive",PathStripToRootW(p) && wide_eq(p,L"C:" BS));
    lstrcpyW(p,BS BS L"srv" BS L"share" BS L"x" BS L"y");
    check("PathStripToRootW share",PathStripToRootW(p) && wide_eq(p,BS BS L"srv" BS L"share"));
    lstrcpyW(p,L"a" BS L"b");
    check("PathStripToRootW without a root",!PathStripToRootW(p));

    lstrcpyW(p,L"C:" BS L"a" BS L"b");
    check("PathAddExtensionW",PathAddExtensionW(p,L".txt") && wide_eq(p,L"C:" BS L"a" BS L"b.txt"));
    check("PathAddExtensionW keeps an extension",!PathAddExtensionW(p,L".bak") && wide_eq(p,L"C:" BS L"a" BS L"b.txt"));
    lstrcpyW(p,L"tool");
    check("PathAddExtensionW default",PathAddExtensionW(p,NULL) && wide_eq(p,L"tool.exe"));
    check("PathRenameExtensionW",PathRenameExtensionW(p,L".bak") && wide_eq(p,L"tool.bak"));
    lstrcpyW(p,L"plain");
    check("PathRenameExtensionW adds one",PathRenameExtensionW(p,L".md") && wide_eq(p,L"plain.md"));

    lstrcpyW(p,L"C:" BS L"Program Files" BS L"x.exe"); PathQuoteSpacesW(p);
    check("PathQuoteSpacesW",wide_eq(p,L"\"C:" BS L"Program Files" BS L"x.exe\""));
    PathUnquoteSpacesW(p);
    check("PathUnquoteSpacesW",wide_eq(p,L"C:" BS L"Program Files" BS L"x.exe"));
    lstrcpyW(p,L"C:" BS L"x.exe"); PathQuoteSpacesW(p);
    check("PathQuoteSpacesW without spaces",wide_eq(p,L"C:" BS L"x.exe"));

    const WCHAR *c1=L"app.exe -a -b", *c2=L"\"my app.exe\" -x", *c3=L"app.exe";
    check("PathGetArgsW",wide_eq(PathGetArgsW(c1),L"-a -b") && wide_eq(PathGetArgsW(c2),L"-x") && *PathGetArgsW(c3)==0);
    lstrcpyW(p,c1); PathRemoveArgsW(p);
    check("PathRemoveArgsW",wide_eq(p,L"app.exe"));
    lstrcpyW(p,c2); PathRemoveArgsW(p);
    check("PathRemoveArgsW keeps a quoted program",wide_eq(p,L"\"my app.exe\""));

    const WCHAR *r1=L"C:" BS L"a" BS L"b", *r2=BS BS L"srv" BS L"share" BS L"x";
    check("PathSkipRootW",wide_eq(PathSkipRootW(r1),L"a" BS L"b") && wide_eq(PathSkipRootW(r2),L"x") && PathSkipRootW(L"a" BS L"b")==NULL);
    check("PathFindNextComponentW",wide_eq(PathFindNextComponentW(r1),L"a" BS L"b") && wide_eq(PathFindNextComponentW(L"a" BS L"b"),L"b")
          && *PathFindNextComponentW(L"b")==0 && PathFindNextComponentW(L"")==NULL);

    check("PathCanonicalizeW",PathCanonicalizeW(p,L"C:" BS L"a" BS L"." BS L"b" BS L".." BS L"c") && wide_eq(p,L"C:" BS L"a" BS L"c"));
    check("PathCanonicalizeW stops at the root",PathCanonicalizeW(p,L"C:" BS L"a" BS L".." BS L"..") && wide_eq(p,L"C:" BS));
    check("PathCanonicalizeW share",PathCanonicalizeW(p,BS BS L"srv" BS L"share" BS L"x" BS L".." BS L"y") && wide_eq(p,BS BS L"srv" BS L"share" BS L"y"));
}

/* The string helpers (shlstr.rs). */
static void more_strings(void) {
    const WCHAR *h=L"hello";
    WCHAR b[32];
    check("StrCmpW orders",StrCmpW(L"a",L"b")<0 && StrCmpW(L"b",L"a")>0 && StrCmpW(L"a",L"a")==0 && StrCmpW(L"a",L"ab")<0);
    check("StrCmpIW ignores case",StrCmpIW(L"HELLO",h)==0 && StrCmpIW(L"hellp",L"HELLO")>0);
    check("StrCmpNW and StrCmpNIW",StrCmpNW(L"abcd",L"abxy",2)==0 && StrCmpNW(L"abcd",L"abxy",3)<0 && StrCmpNIW(L"ABcd",L"abXY",2)==0);
    check("ChrCmpIW",ChrCmpIW(L'a',L'A')==0 && ChrCmpIW(L'a',L'b')!=0);
    check("StrChrW and StrRChrW",StrChrW(h,L'l')==h+2 && StrRChrW(h,NULL,L'l')==h+3 && StrChrW(h,L'z')==NULL);
    check("StrRChrW before an end",StrRChrW(h,h+3,L'l')==h+2);
    check("StrChrIW and StrRChrIW",StrChrIW(L"Hello",L'h')!=NULL && StrChrW(L"Hello",L'h')==NULL && StrRChrIW(h,NULL,L'L')==h+3);
    const WCHAR *w=L"hello world";
    check("StrStrW is case sensitive",StrStrW(w,L"o w")==w+4 && StrStrW(w,L"O W")==NULL && StrStrW(w,L"")==NULL);
    const WCHAR *r=L"abcABC";
    check("StrRStrIW finds the last",StrRStrIW(r,NULL,L"bc")==r+4 && StrRStrIW(r,r+4,L"bc")==r+1 && StrRStrIW(r,NULL,L"x")==NULL);
    check("StrSpnW, StrCSpnW, StrPBrkW",StrSpnW(L"aabbc",L"ab")==4 && StrCSpnW(h,L"l")==2 && StrPBrkW(h,L"xl")==h+2 && StrPBrkW(h,L"xyz")==NULL);

    check("StrToIntW",StrToIntW(L"123abc")==123 && StrToIntW(L"-45")==-45 && StrToIntW(L"abc")==0);
    int v=7; LONGLONG q=7;
    check("StrToIntExW decimal",StrToIntExW(L"  -12",STIF_DEFAULT,&v) && v==-12);
    check("StrToIntExW hexadecimal",StrToIntExW(L" 0x1F",STIF_SUPPORT_HEX,&v) && v==31);
    check("StrToIntExW without digits",!StrToIntExW(L"x",STIF_DEFAULT,&v));
    check("StrToInt64ExW",StrToInt64ExW(L"9999999999",STIF_DEFAULT,&q) && q==9999999999LL);

    LPWSTR d=StrDupW(h);
    check("StrDupW",d && d!=h && wide_eq(d,h));
    if(d) LocalFree(d);
    lstrcpyW(b,L"x");
    check("StrCpyW and StrCatW",StrCpyW(b,L"ab")==b && StrCatW(b,L"cd")==b && wide_eq(b,L"abcd"));
    check("StrCpyNW truncates and terminates",StrCpyNW(b,L"abcdef",4)==b && wide_eq(b,L"abc"));
    lstrcpyW(b,L"ab");
    check("StrCatBuffW fills the buffer",StrCatBuffW(b,L"cdef",5)==b && wide_eq(b,L"abcd"));
    lstrcpyW(b,L" \tx y \t");
    check("StrTrimW",StrTrimW(b,L" \t") && wide_eq(b,L"x y") && !StrTrimW(b,L" \t"));

    /* The examples of the StrFormatByteSize documentation. */
    check("532 bytes",StrFormatByteSizeW(532,b,32)==b && wide_eq(b,L"532 bytes"));
    StrFormatByteSizeW(1340,b,32); check("1.30 KB",wide_eq(b,L"1.30 KB"));
    StrFormatByteSizeW(23506,b,32); check("22.9 KB",wide_eq(b,L"22.9 KB"));
    StrFormatByteSizeW(2400016,b,32); check("2.28 MB",wide_eq(b,L"2.28 MB"));
    StrFormatByteSizeW(2400000000LL,b,32); check("2.23 GB",wide_eq(b,L"2.23 GB"));
    StrFormatByteSizeW(1024,b,32); check("1.00 KB",wide_eq(b,L"1.00 KB"));
    StrFormatByteSizeW(1023,b,32); check("1023 bytes",wide_eq(b,L"1023 bytes"));
    StrFormatByteSizeW(104857600,b,32); check("100 MB",wide_eq(b,L"100 MB"));
}

/* The ANSI forms. */
static void ansi_forms(void) {
    const char *n="C:\\a\\b.txt", *h="Hello World";
    char p[MAX_PATH];
    check("PathFindFileNameA and PathFindExtensionA",str_eq(PathFindFileNameA(n),"b.txt") && str_eq(PathFindExtensionA(n),".txt"));
    check("PathIsRelativeA, PathIsRootA, PathIsUNCA, PathIsURLA",
          PathIsRelativeA("a\\b") && !PathIsRelativeA(n) && PathIsRootA("C:\\") && PathIsUNCA("\\\\srv\\s") && PathIsURLA("https://x"));
    lstrcpyA(p,"C:\\a");
    check("PathAddBackslashA",PathAddBackslashA(p)==p+5 && str_eq(p,"C:\\a\\"));
    PathRemoveBackslashA(p);
    check("PathRemoveBackslashA",str_eq(p,"C:\\a"));
    lstrcpyA(p,n); PathStripPathA(p);
    check("PathStripPathA",str_eq(p,"b.txt"));
    check("PathRenameExtensionA",PathRenameExtensionA(p,".bak") && str_eq(p,"b.bak"));
    lstrcpyA(p,"C:\\my dir\\x"); PathQuoteSpacesA(p);
    check("PathQuoteSpacesA",str_eq(p,"\"C:\\my dir\\x\""));
    PathUnquoteSpacesA(p);
    check("PathUnquoteSpacesA",str_eq(p,"C:\\my dir\\x"));
    check("PathCanonicalizeA",PathCanonicalizeA(p,"C:\\a\\..\\b") && str_eq(p,"C:\\b"));
    check("PathGetArgsA",str_eq(PathGetArgsA("app.exe -q"),"-q"));
    check("StrStrA and StrStrIA",StrStrA(h,"World")==h+6 && StrStrA(h,"WORLD")==NULL && StrStrIA(h,"WORLD")==h+6);
    check("StrChrA and StrRChrA",StrChrA(h,'o')==h+4 && StrRChrA(h,NULL,'o')==h+7 && StrChrIA(h,'w')==h+6);
    check("StrCmpNA and StrCmpNIA",StrCmpNA("abc","abd",2)==0 && StrCmpNA("abc","abd",3)<0 && StrCmpNIA("ABC","abd",2)==0);
    int v=0;
    check("StrToIntA and StrToIntExA",StrToIntA("77x")==77 && StrToIntExA("0x10",STIF_SUPPORT_HEX,&v) && v==16);
    lstrcpyA(p,"  x  ");
    check("StrTrimA",StrTrimA(p," ") && str_eq(p,"x"));
    char *d=StrDupA(h);
    check("StrDupA",d && d!=h && str_eq(d,h));
    if(d) LocalFree(d);
    StrFormatByteSizeA(1340,p,32);
    check("StrFormatByteSizeA",str_eq(p,"1.30 KB"));
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
    more_paths();
    more_strings();
    ansi_forms();
    is_os();
    finish("shlpath");
}
