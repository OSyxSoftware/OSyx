/* SECURITY_ATTRIBUTES with a descriptor on sections and new directories;
   also runs on Windows. */
#include "common.h"
#include <aclapi.h>

static void sections(SECURITY_ATTRIBUTES *sa) {
    HANDLE a=CreateFileMappingW(INVALID_HANDLE_VALUE,sa,PAGE_READWRITE,0,4096,NULL);
    check("unnamed section with a descriptor",a!=NULL);
    if(a) {
        void *v=MapViewOfFile(a,FILE_MAP_WRITE,0,0,0);
        check("its view is usable",v!=NULL);
        if(v){ *(volatile int *)v=7; UnmapViewOfFile(v); }
        CloseHandle(a);
    }
    HANDLE b=CreateFileMappingW(INVALID_HANDLE_VALUE,sa,PAGE_READWRITE,0,4096,L"osyx-secattr-section");
    check("named section with a descriptor",b!=NULL);
    if(b) {
        PACL dacl=NULL; PSECURITY_DESCRIPTOR sd=NULL;
        DWORD r=GetSecurityInfo(b,SE_KERNEL_OBJECT,DACL_SECURITY_INFORMATION,NULL,NULL,&dacl,NULL,&sd);
        check("named section keeps its DACL",r==ERROR_SUCCESS && dacl && dacl->AceCount==1);
        if(sd) LocalFree(sd);
        CloseHandle(b);
    }
}

static void directories(SECURITY_ATTRIBUTES *sa) {
    WCHAR dir[MAX_PATH]; DWORD n=GetCurrentDirectoryW(MAX_PATH-2,dir); if(n) lstrcatW(dir,L"\\");
    if(!n || n>MAX_PATH-40) { check("temp path",0); return; }
    lstrcatW(dir,L"osyx-secattr-dir");
    RemoveDirectoryW(dir);
    BOOL made=CreateDirectoryW(dir,sa);
    if(!made){put("       error ");put_uint(GetLastError());put("\n");}
    check("directory with a descriptor",made);
    check("directory exists",GetFileAttributesW(dir)!=INVALID_FILE_ATTRIBUTES && (GetFileAttributesW(dir)&FILE_ATTRIBUTE_DIRECTORY));
    check("remove it",RemoveDirectoryW(dir));
}

void __stdcall start(void) {
    /* Full access for the creator owner's user and inheritable to children. */
    HANDLE token; DWORD len=0; BYTE user[256];
    OpenProcessToken(GetCurrentProcess(),TOKEN_QUERY,&token);
    check("token user",GetTokenInformation(token,TokenUser,user,sizeof user,&len));
    CloseHandle(token);
    BYTE acl_buf[256]; PACL acl=(PACL)acl_buf;
    check("initialize ACL",InitializeAcl(acl,sizeof acl_buf,ACL_REVISION));
    check("allow the user",AddAccessAllowedAceEx(acl,ACL_REVISION,OBJECT_INHERIT_ACE|CONTAINER_INHERIT_ACE,GENERIC_ALL,((TOKEN_USER *)user)->User.Sid));
    SECURITY_DESCRIPTOR sd;
    check("initialize descriptor",InitializeSecurityDescriptor(&sd,SECURITY_DESCRIPTOR_REVISION));
    check("set DACL",SetSecurityDescriptorDacl(&sd,TRUE,acl,FALSE));
    SECURITY_ATTRIBUTES sa={sizeof sa,&sd,FALSE};
    sections(&sa);
    directories(&sa);
    finish("secattr");
}
