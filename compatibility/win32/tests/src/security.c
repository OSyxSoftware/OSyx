/* Public Win64 layouts and observable SID/ACL/descriptor/token behaviour.
 * The same PE runs on Windows; OSyx-specific identity/unsupported checks
 * are gated by the runtime's OSYX environment variable. */
#include "common.h"
_Static_assert(sizeof(SECURITY_DESCRIPTOR)==40,"Win64 descriptor ABI");
_Static_assert(sizeof(SID_AND_ATTRIBUTES)==16,"Win64 token ABI");
_Static_assert(sizeof(ACL)==8,"ACL ABI");
static BYTE world[SECURITY_MAX_SID_SIZE], users[SECURITY_MAX_SID_SIZE], aclbuf[128];
static BYTE *token_data(HANDLE token,TOKEN_INFORMATION_CLASS kind,DWORD *needed) {
    *needed=0;
    if(GetTokenInformation(token,kind,NULL,0,needed)||GetLastError()!=ERROR_INSUFFICIENT_BUFFER) return NULL;
    BYTE *p=HeapAlloc(GetProcessHeap(),HEAP_ZERO_MEMORY,*needed);
    if(!p||!GetTokenInformation(token,kind,p,*needed,needed)) { if(p)HeapFree(GetProcessHeap(),0,p);return NULL; }
    return p;
}
void __stdcall start(void) {
    char marker[16];int native=GetEnvironmentVariableA("OSYX",marker,sizeof marker)>0;
    DWORD size=0;
    check("well-known SID size query",!CreateWellKnownSid(WinWorldSid,NULL,NULL,&size)&&size==12&&GetLastError()==ERROR_INSUFFICIENT_BUFFER);
    size=sizeof world;check("create actual World SID",CreateWellKnownSid(WinWorldSid,NULL,world,&size)&&size==12&&IsValidSid(world));
    check("World SID authority and RID",GetSidIdentifierAuthority(world)->Value[5]==1&&*GetSidSubAuthorityCount(world)==1&&*GetSidSubAuthority(world,0)==0);
    check("SID lengths use encoded subauthority count",GetLengthSid(world)==12&&GetSidLengthRequired(2)==16);
    size=sizeof users;check("create Builtin Users SID",CreateWellKnownSid(WinBuiltinUsersSid,NULL,users,&size)&&size==16);
    BYTE manual[SECURITY_MAX_SID_SIZE]={0};SID_IDENTIFIER_AUTHORITY nt=SECURITY_NT_AUTHORITY;
    BOOL made=InitializeSid(manual,&nt,2);*GetSidSubAuthority(manual,0)=32;*GetSidSubAuthority(manual,1)=545;
    check("independently built SID equals well-known alias",made&&EqualSid(users,manual)&&IsWellKnownSid(manual,WinBuiltinUsersSid)&&!EqualSid(world,manual));
    BYTE copy[SECURITY_MAX_SID_SIZE];memset(copy,0xa5,sizeof copy);
    check("CopySid honors capacity",!CopySid(4,copy,users)&&GetLastError()==ERROR_INSUFFICIENT_BUFFER&&copy[0]==0xa5);
    check("CopySid copies only SID bytes",CopySid(sizeof copy,copy,users)&&EqualSid(copy,users)&&copy[16]==0xa5);
    copy[0]=2;check("invalid SID revision rejected",!IsValidSid(copy));copy[0]=1;copy[1]=16;
    check("too many SID subauthorities rejected",!IsValidSid(copy));
    BYTE domain[SECURITY_MAX_SID_SIZE];InitializeSid(domain,&nt,4);
    for(DWORD i=0;i<4;i++)*GetSidSubAuthority(domain,i)=i?i:21;
    size=sizeof copy;
    check("account SID uses provided domain",CreateWellKnownSid(WinAccountDomainUsersSid,domain,copy,&size)&&size==28&&*GetSidSubAuthorityCount(copy)==5&&*GetSidSubAuthority(copy,4)==513);
    PACL acl=(PACL)aclbuf;
    check("initialize empty ACL",InitializeAcl(acl,sizeof aclbuf,ACL_REVISION)&&IsValidAcl(acl)&&acl->AceCount==0&&acl->AclSize==sizeof aclbuf);
    check("append denied and allowed ACEs",AddAccessDeniedAce(acl,ACL_REVISION,GENERIC_WRITE,world)&&AddAccessAllowedAce(acl,ACL_REVISION,GENERIC_READ,users)&&acl->AceCount==2);
    PVOID ace=NULL;BOOL got=GetAce(acl,0,&ace);ACCESS_DENIED_ACE *deny=ace;
    check("denied ACE preserves type, mask and SID",got&&deny->Header.AceType==ACCESS_DENIED_ACE_TYPE&&deny->Mask==GENERIC_WRITE&&EqualSid(&deny->SidStart,world));
    got=GetAce(acl,1,&ace);ACCESS_ALLOWED_ACE *allow=ace;
    check("allowed ACE preserves type, mask and SID",got&&allow->Header.AceType==ACCESS_ALLOWED_ACE_TYPE&&allow->Mask==GENERIC_READ&&EqualSid(&allow->SidStart,users));
    check("GetAce rejects out-of-range index",!GetAce(acl,2,&ace)&&GetLastError()==ERROR_INVALID_PARAMETER);
    ACL tiny;InitializeAcl(&tiny,sizeof tiny,ACL_REVISION);
    check("ACL exhaustion fails without changing count",!AddAccessAllowedAce(&tiny,ACL_REVISION,1,world)&&GetLastError()==ERROR_ALLOTTED_SPACE_EXCEEDED&&tiny.AceCount==0);
    BOOL invalid_revision=InitializeAcl(&tiny,sizeof tiny,6);DWORD revision_error=GetLastError();
    check("unknown ACL revision rejected",!invalid_revision&&revision_error==ERROR_INVALID_PARAMETER);
    tiny.AceCount=1;check("truncated ACE list rejected",!IsValidAcl(&tiny));
    SECURITY_DESCRIPTOR sd;
    check("initialize absolute security descriptor",InitializeSecurityDescriptor(&sd,SECURITY_DESCRIPTOR_REVISION)&&IsValidSecurityDescriptor(&sd));
    check("set real owner group and DACL",SetSecurityDescriptorOwner(&sd,world,TRUE)&&SetSecurityDescriptorGroup(&sd,users,FALSE)&&SetSecurityDescriptorDacl(&sd,TRUE,acl,FALSE));
    PSID owner=NULL,group=NULL;BOOL defaulted=FALSE,present=FALSE;PACL actual=NULL;
    check("read owner and defaulted flag",GetSecurityDescriptorOwner(&sd,&owner,&defaulted)&&owner==world&&defaulted);
    check("read primary group",GetSecurityDescriptorGroup(&sd,&group,&defaulted)&&group==users&&!defaulted);
    check("read actual DACL",GetSecurityDescriptorDacl(&sd,&present,&actual,&defaulted)&&present&&actual==acl&&!defaulted);
    SECURITY_DESCRIPTOR_CONTROL control=0;DWORD revision=0;
    check("descriptor control flags match stored components",GetSecurityDescriptorControl(&sd,&control,&revision)&&control==(SE_OWNER_DEFAULTED|SE_DACL_PRESENT)&&revision==1);
    check("null DACL remains distinguishable from an absent DACL",SetSecurityDescriptorDacl(&sd,TRUE,NULL,FALSE)&&GetSecurityDescriptorDacl(&sd,&present,&actual,&defaulted)&&present&&actual==NULL);
    check("absent DACL clears presence",SetSecurityDescriptorDacl(&sd,FALSE,NULL,FALSE)&&GetSecurityDescriptorDacl(&sd,&present,&actual,&defaulted)&&!present);
    SECURITY_ATTRIBUTES attributes={sizeof(SECURITY_ATTRIBUTES),NULL,FALSE};
    HANDLE event=CreateEventW(&attributes,FALSE,FALSE,NULL);
    check("default security attributes create a real event",event!=NULL);if(event)CloseHandle(event);
    if(native) {
        attributes.lpSecurityDescriptor=&sd;
        check("custom event security is not silently ignored",!CreateEventW(&attributes,FALSE,FALSE,NULL)&&GetLastError()==ERROR_NOT_SUPPORTED);
        HANDLE file=CreateFileW(L"C:\\tmp\\security-must-not-create",GENERIC_WRITE,0,&attributes,CREATE_ALWAYS,0,NULL);
        check("custom file security fails before creating a file",file==INVALID_HANDLE_VALUE&&GetLastError()==ERROR_NOT_SUPPORTED&&GetFileAttributesW(L"C:\\tmp\\security-must-not-create")==INVALID_FILE_ATTRIBUTES);
        check("custom mutex security fails explicitly",!CreateMutexW(&attributes,FALSE,NULL)&&GetLastError()==ERROR_NOT_SUPPORTED);
        check("custom semaphore security fails explicitly",!CreateSemaphoreW(&attributes,0,1,NULL)&&GetLastError()==ERROR_NOT_SUPPORTED);
    }
    BYTE relative[64]={0};SECURITY_DESCRIPTOR_RELATIVE *rel=(void *)relative;
    rel->Revision=1;rel->Control=SE_SELF_RELATIVE|SE_DACL_PRESENT;rel->Owner=20;rel->Dacl=32;
    memcpy(relative+20,world,12);InitializeAcl((PACL)(relative+32),8,ACL_REVISION);
    check("self-relative descriptor components use offsets",IsValidSecurityDescriptor(rel)&&GetSecurityDescriptorOwner(rel,&owner,&defaulted)&&owner==relative+20&&GetSecurityDescriptorDacl(rel,&present,&actual,&defaulted)&&present&&(BYTE *)actual==relative+32);
    BOOL relative_set=SetSecurityDescriptorOwner(rel,users,FALSE);DWORD relative_error=GetLastError();
    check("absolute setter rejects self-relative descriptor",!relative_set&&relative_error==ERROR_INVALID_SECURITY_DESCR);
    HANDLE token=NULL;check("open current process token",OpenProcessToken(GetCurrentProcess(),TOKEN_QUERY,&token));
    DWORD needed=0;BYTE *data=token_data(token,TokenUser,&needed);TOKEN_USER *user=(void *)data;
    check("token user contains valid in-buffer SID",data&&needed>=sizeof(TOKEN_USER)+8&&(BYTE *)user->User.Sid>=data+sizeof(TOKEN_USER)&&(BYTE *)user->User.Sid+GetLengthSid(user->User.Sid)<=data+needed&&IsValidSid(user->User.Sid));
    if(native&&data) {
        DWORD expected=0;char text[16];if(GetEnvironmentVariableA("WIN_EXPECT_UID",text,sizeof text))for(int i=0;text[i]>='0'&&text[i]<='9';i++)expected=expected*10+(DWORD)(text[i]-'0');
        check("token UID is the real OSyx credential",GetSidIdentifierAuthority(user->User.Sid)->Value[5]==22&&*GetSidSubAuthorityCount(user->User.Sid)==2&&*GetSidSubAuthority(user->User.Sid,0)==1&&*GetSidSubAuthority(user->User.Sid,1)==expected);
    }
    if(data)HeapFree(GetProcessHeap(),0,data);
    data=token_data(token,TokenOwner,&needed);check("token owner is a valid SID",data&&IsValidSid(((TOKEN_OWNER *)data)->Owner));if(data)HeapFree(GetProcessHeap(),0,data);
    data=token_data(token,TokenPrimaryGroup,&needed);check("token primary group is a valid SID",data&&IsValidSid(((TOKEN_PRIMARY_GROUP *)data)->PrimaryGroup));if(data)HeapFree(GetProcessHeap(),0,data);
    data=token_data(token,TokenGroups,&needed);BOOL groups_ok=data!=NULL;DWORD world_seen=0;
    if(data) { TOKEN_GROUPS *groups=(void *)data;for(DWORD i=0;i<groups->GroupCount;i++) { BYTE *p=groups->Groups[i].Sid;groups_ok&=p>=data&&p+GetLengthSid(p)<=data+needed&&IsValidSid(p);world_seen|=EqualSid(p,world); } }
    check("token groups contain actual SID records and World",groups_ok&&world_seen);if(data)HeapFree(GetProcessHeap(),0,data);
    TOKEN_TYPE type=0;check("process token is primary",GetTokenInformation(token,TokenType,&type,sizeof type,&needed)&&type==TokenPrimary&&needed==sizeof type);
    BYTE guard[16];memset(guard,0xa5,sizeof guard);check("token size failure does not write output",!GetTokenInformation(token,TokenUser,guard,1,&needed)&&GetLastError()==ERROR_INSUFFICIENT_BUFFER&&guard[0]==0xa5);
    HANDLE limited=NULL;check("token duplication can drop query access",DuplicateHandle(GetCurrentProcess(),token,GetCurrentProcess(),&limited,0,FALSE,0)&&!GetTokenInformation(limited,TokenType,&type,sizeof type,&needed)&&GetLastError()==ERROR_ACCESS_DENIED);
    CloseHandle(limited);HANDLE duplicate=NULL;
    check("same-access duplicate survives original close",DuplicateHandle(GetCurrentProcess(),token,GetCurrentProcess(),&duplicate,0,FALSE,DUPLICATE_SAME_ACCESS)&&CloseHandle(token)&&GetTokenInformation(duplicate,TokenType,&type,sizeof type,&needed));
    if(native)check("unsupported token classes fail explicitly",!GetTokenInformation(duplicate,TokenAuditPolicy,NULL,0,&needed)&&GetLastError()==ERROR_NOT_SUPPORTED);
    CloseHandle(duplicate);check("closed token handle is rejected",!GetTokenInformation(duplicate,TokenType,&type,sizeof type,&needed)&&GetLastError()==ERROR_INVALID_HANDLE);
    finish("security");
}
