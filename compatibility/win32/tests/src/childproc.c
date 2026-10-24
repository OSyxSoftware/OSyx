/* Handles across processes: inheritance (bInheritHandles with
   PROC_THREAD_ATTRIBUTE_HANDLE_LIST), HANDLE_FLAG_INHERIT, and
   DuplicateHandle into and out of another process; also runs on Windows.
   The program runs itself as the child with "--child <section> <other>
   <parent pid>" (hex). */
#include "common.h"

static const char message[]="inherited section";
static const char reply[]="from the child";

static unsigned long long hex(const WCHAR **p) {
    unsigned long long v=0;
    while(**p==L' ') (*p)++;
    for(;;(*p)++) {
        WCHAR c=**p;
        if(c>=L'0'&&c<=L'9') v=v*16+(c-L'0');
        else if(c>=L'a'&&c<=L'f') v=v*16+(c-L'a'+10);
        else break;
    }
    return v;
}

static void put_hex(WCHAR *s, int *n, unsigned long long v) {
    WCHAR t[20]; int k=0;
    do { int d=(int)(v&15); t[k++]=(WCHAR)(d<10?L'0'+d:L'a'+d-10); v>>=4; } while(v);
    s[(*n)++]=L' ';
    while(k) s[(*n)++]=t[--k];
}

static int same(const char *a, const char *b) { while(*b) if(*a++!=*b++) return 0; return *a==0; }

static void copy(char *d, const char *s) { while((*d++=*s++)) {} }

static void section_path(WCHAR *out, const WCHAR *leaf) {
    DWORD n=GetTempPathW(MAX_PATH,out);
    while(*leaf) out[n++]=*leaf++;
    out[n]=0;
}

static HANDLE file_section(const WCHAR *leaf, int create) {
    WCHAR path[MAX_PATH]; section_path(path,leaf);
    HANDLE f=CreateFileW(path,GENERIC_READ|GENERIC_WRITE,FILE_SHARE_READ|FILE_SHARE_WRITE|FILE_SHARE_DELETE,NULL,create?CREATE_ALWAYS:OPEN_EXISTING,0,NULL);
    if(f==INVALID_HANDLE_VALUE) return NULL;
    HANDLE section=CreateFileMappingW(f,NULL,PAGE_READWRITE,0,4096,NULL);
    CloseHandle(f);
    return section;
}

/* The child: map the inherited section, take the parent's other section,
   and give the parent a section of its own (its handle is the exit code). */
static DWORD child(const WCHAR *args) {
    HANDLE inherited=(HANDLE)(UINT_PTR)hex(&args), other=(HANDLE)(UINT_PTR)hex(&args);
    DWORD parent_pid=(DWORD)hex(&args);
    HANDLE client=(HANDLE)(UINT_PTR)hex(&args);
    HANDLE done=(HANDLE)(UINT_PTR)hex(&args), go=(HANDLE)(UINT_PTR)hex(&args);
    /* The parent sets `go` after starting us; we set `done` at the end. */
    if(WaitForSingleObject(go,10000)!=WAIT_OBJECT_0) return 10;
    DWORD wrote=0;
    if(!WriteFile(client,"ping",4,&wrote,NULL) || wrote!=4) return 9;
    DWORD flags=0;
    if(!GetHandleInformation(inherited,&flags) || !(flags&HANDLE_FLAG_INHERIT)) return 2;
    char *v=MapViewOfFile(inherited,FILE_MAP_WRITE,0,0,0);
    if(!v || !same(v,message)) return 3;
    copy(v,reply);
    UnmapViewOfFile(v);
    HANDLE parent=OpenProcess(PROCESS_DUP_HANDLE,FALSE,parent_pid);
    if(!parent) return 4;
    HANDLE mine=NULL;
    if(!DuplicateHandle(parent,other,GetCurrentProcess(),&mine,0,FALSE,DUPLICATE_SAME_ACCESS) || !mine) return 5;
    v=MapViewOfFile(mine,FILE_MAP_READ,0,0,0);
    if(!v || !same(v,"not inherited")) return 6;
    UnmapViewOfFile(v);
    HANDLE independent=file_section(L"osyx-child-section-other.bin",0);
    char *shared=MapViewOfFile(mine,FILE_MAP_WRITE,0,0,0);
    char *alias=independent?MapViewOfFile(independent,FILE_MAP_WRITE,0,0,0):NULL;
    if(!shared || !alias) return 12;
    shared[100]=51; alias[101]=52;
    if(alias[100]!=51 || shared[101]!=52) return 13;
    UnmapViewOfFile(shared); UnmapViewOfFile(alias); CloseHandle(independent); CloseHandle(mine);
    HANDLE back=CreateFileMappingW(INVALID_HANDLE_VALUE,NULL,PAGE_READWRITE,0,4096,NULL);
    v=back?MapViewOfFile(back,FILE_MAP_WRITE,0,0,0):NULL;
    if(!v) return 7;
    copy(v,"pushed to the parent");
    UnmapViewOfFile(v);
    HANDLE in_parent=NULL;
    if(!DuplicateHandle(GetCurrentProcess(),back,parent,&in_parent,0,FALSE,DUPLICATE_SAME_ACCESS|DUPLICATE_CLOSE_SOURCE) || !in_parent) return 8;
    if(!SetEvent(done)) return 11;
    return (DWORD)(UINT_PTR)in_parent;
}

void __stdcall start(void) {
    const WCHAR *cmd=GetCommandLineW(), *at=cmd;
    for(;*at;at++) if(at[0]==L'-'&&at[1]==L'-'&&at[2]==L'c'&&at[3]==L'h'&&at[4]==L'i'&&at[5]==L'l'&&at[6]==L'd') ExitProcess(child(at+7));

    HANDLE s=file_section(L"osyx-child-section-inherit.bin",1);
    HANDLE other=file_section(L"osyx-child-section-other.bin",1);
    check("sections",s && other);
    if(!s || !other) finish("childproc");
    char *v=MapViewOfFile(s,FILE_MAP_WRITE,0,0,0); copy(v,message); UnmapViewOfFile(v);
    v=MapViewOfFile(other,FILE_MAP_WRITE,0,0,0); copy(v,"not inherited"); UnmapViewOfFile(v);
    DWORD flags=7;
    check("handles start not inheritable",GetHandleInformation(s,&flags) && flags==0);
    check("mark inheritable",SetHandleInformation(s,HANDLE_FLAG_INHERIT,HANDLE_FLAG_INHERIT));
    check("flag reads back",GetHandleInformation(s,&flags) && flags==HANDLE_FLAG_INHERIT);

    WCHAR exe[MAX_PATH]; GetModuleFileNameW(NULL,exe,MAX_PATH);
    WCHAR line[MAX_PATH+80]; int n=0;
    for(int i=0;exe[i];i++) line[n++]=exe[i];
    const WCHAR *tag=L" --child";
    for(int i=0;tag[i];i++) line[n++]=tag[i];
    HANDLE server=CreateNamedPipeW(L"\\\\.\\pipe\\osyx-childproc",PIPE_ACCESS_DUPLEX,PIPE_TYPE_BYTE|PIPE_READMODE_BYTE|PIPE_WAIT,1,4096,4096,0,NULL);
    HANDLE client=CreateFileW(L"\\\\.\\pipe\\osyx-childproc",GENERIC_READ|GENERIC_WRITE,0,NULL,OPEN_EXISTING,0,NULL);
    check("pipe pair",server!=INVALID_HANDLE_VALUE && client!=INVALID_HANDLE_VALUE);
    check("server sees the client",ConnectNamedPipe(server,NULL) || GetLastError()==ERROR_PIPE_CONNECTED);
    check("mark the client inheritable",SetHandleInformation(client,HANDLE_FLAG_INHERIT,HANDLE_FLAG_INHERIT));
    SECURITY_ATTRIBUTES inh={sizeof inh,NULL,TRUE};
    HANDLE done=CreateEventW(&inh,TRUE,FALSE,NULL), go=CreateEventW(&inh,FALSE,FALSE,NULL);
    check("inheritable events",done && go);
    put_hex(line,&n,(UINT_PTR)s); put_hex(line,&n,(UINT_PTR)other); put_hex(line,&n,GetCurrentProcessId()); put_hex(line,&n,(UINT_PTR)client); put_hex(line,&n,(UINT_PTR)done); put_hex(line,&n,(UINT_PTR)go);
    line[n]=0;

    SIZE_T size=0;
    InitializeProcThreadAttributeList(NULL,1,0,&size);
    static BYTE list_buf[256];
    LPPROC_THREAD_ATTRIBUTE_LIST list=(LPPROC_THREAD_ATTRIBUTE_LIST)list_buf;
    check("attribute list",size<=sizeof list_buf && InitializeProcThreadAttributeList(list,1,0,&size));
    HANDLE pass[4]={s,client,done,go};
    check("handle list",UpdateProcThreadAttribute(list,0,PROC_THREAD_ATTRIBUTE_HANDLE_LIST,pass,sizeof pass,NULL,NULL));
    STARTUPINFOEXW si={0}; si.StartupInfo.cb=sizeof si; si.lpAttributeList=list;
    PROCESS_INFORMATION pi={0};
    BOOL ok=CreateProcessW(exe,line,NULL,NULL,TRUE,EXTENDED_STARTUPINFO_PRESENT,NULL,NULL,&si.StartupInfo,&pi);
    check("start the child",ok);
    DWORD code=0;
    if(ok) {
        Sleep(50);
        check("set the child's event",SetEvent(go));
        check("the child sets an inherited event",WaitForSingleObject(done,20000)==WAIT_OBJECT_0);
        check("child finishes",WaitForSingleObject(pi.hProcess,20000)==WAIT_OBJECT_0 && GetExitCodeProcess(pi.hProcess,&code));
        check("child's checks pass",code>=0x10);
        if(code<0x10){ put("       child exit "); put_uint(code); put("\n"); }
        CloseHandle(pi.hThread); CloseHandle(pi.hProcess);
    }
    DeleteProcThreadAttributeList(list);
    CloseHandle(client); /* the child's copy stays connected */
    char got[8]={0}; DWORD read=0;
    check("the child wrote to the inherited pipe",ReadFile(server,got,4,&read,NULL) && read==4 && got[0]=='p' && got[3]=='g');
    CloseHandle(server);
    v=MapViewOfFile(s,FILE_MAP_READ,0,0,0);
    check("the child wrote to the inherited section",v && same(v,reply));
    if(v) UnmapViewOfFile(v);
    HANDLE pushed=(HANDLE)(UINT_PTR)code;
    v=code>=0x10?MapViewOfFile(pushed,FILE_MAP_READ,0,0,0):NULL;
    check("the child's section arrived in this process",v && same(v,"pushed to the parent"));
    if(v) UnmapViewOfFile(v);
    CloseHandle(s); CloseHandle(other);
    WCHAR path[MAX_PATH]; section_path(path,L"osyx-child-section-inherit.bin"); DeleteFileW(path);
    section_path(path,L"osyx-child-section-other.bin"); DeleteFileW(path);
    finish("childproc");
}
