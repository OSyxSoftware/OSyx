/* Message-queue waits (MsgWaitForMultipleObjectsEx, GetQueueStatus) and
   window property stores (SHGetPropertyStoreForWindow); also runs on Windows. */
#define COBJMACROS
#include "common.h"
#include <shobjidl.h>
_Static_assert(sizeof(PROPERTYKEY)==20 && sizeof(PROPVARIANT)==24,"property store ABI");

static const GUID iid_store={0x886d8eeb,0x8cf2,0x4446,{0x8d,0x02,0xcd,0xba,0x1d,0xbd,0xcf,0x99}};
static const GUID iid_unknown={0,0,0,{0xc0,0,0,0,0,0,0,0x46}};
static const GUID iid_bogus={1,2,3,{4,5,6,7,8,9,10,11}};
/* PKEY_AppUserModel_ID */
static const PROPERTYKEY key_appid={{0x9F4C2855,0x9F79,0x4B39,{0xA8,0xD0,0xE1,0xD4,0x2D,0xE1,0xD5,0xF3}},5};
static const PROPERTYKEY key_other={{0x12345678,0x1234,0x1234,{1,2,3,4,5,6,7,8}},2};

static LRESULT CALLBACK proc(HWND w, UINT m, WPARAM wp, LPARAM lp) { return DefWindowProcW(w,m,wp,lp); }

static HWND make_window(void) {
    return CreateWindowExW(0,L"msgwait",L"msgwait",WS_OVERLAPPED,0,0,32,32,NULL,NULL,GetModuleHandleW(NULL),NULL);
}

static int wide_eq(const WCHAR *a, const WCHAR *b) {
    while(*a && *a==*b){a++;b++;}
    return *a==*b;
}

static int ansi_eq(const char *a, const char *b) {
    while(*a && *a==*b){a++;b++;}
    return *a==*b;
}

static void drain(void) { MSG m; while(PeekMessageW(&m,NULL,0,0,PM_REMOVE)) {} }

static DWORD WINAPI poster(void *arg) {
    Sleep(100);
    return PostMessageW((HWND)arg,WM_USER+7,7,0) ? 0 : 1;
}

static void queue_waits(HWND w) {
    drain();
    check_eq("empty queue: MsgWait times out",MsgWaitForMultipleObjectsEx(0,NULL,0,QS_ALLINPUT,MWMO_INPUTAVAILABLE),WAIT_TIMEOUT);
    check_eq("empty queue status",GetQueueStatus(QS_ALLINPUT),0);
    check("PostMessage to the thread",PostMessageW(NULL,WM_USER,1,2));
    check_eq("new post: current and new bits",GetQueueStatus(QS_POSTMESSAGE),(QS_POSTMESSAGE<<16)|QS_POSTMESSAGE);
    check_eq("seen post: current bits only",GetQueueStatus(QS_POSTMESSAGE),QS_POSTMESSAGE<<16);
    check_eq("seen post does not end a plain MsgWait",MsgWaitForMultipleObjectsEx(0,NULL,0,QS_ALLINPUT,0),WAIT_TIMEOUT);
    check_eq("MWMO_INPUTAVAILABLE sees the waiting post",MsgWaitForMultipleObjectsEx(0,NULL,0,QS_ALLINPUT,MWMO_INPUTAVAILABLE),WAIT_OBJECT_0);
    check_eq("mask without posts ignores it",MsgWaitForMultipleObjectsEx(0,NULL,0,QS_KEY,MWMO_INPUTAVAILABLE),WAIT_TIMEOUT);
    MSG m={0};
    check("posted message is retrieved",PeekMessageW(&m,NULL,0,0,PM_REMOVE) && m.message==WM_USER && m.wParam==1 && m.lParam==2);
    check_eq("retrieved post leaves the queue",GetQueueStatus(QS_POSTMESSAGE),0);

    HANDLE ev=CreateEventW(NULL,TRUE,FALSE,NULL), evs[2]={ev,ev};
    check_eq("unsignalled event times out",MsgWaitForMultipleObjects(1,&ev,FALSE,0,QS_ALLINPUT),WAIT_TIMEOUT);
    SetEvent(ev);
    check_eq("signalled event wins",MsgWaitForMultipleObjects(1,&ev,FALSE,0,QS_ALLINPUT),WAIT_OBJECT_0);
    ResetEvent(ev);
    check("PostMessage to the window",PostMessageW(w,WM_USER+1,0,0));
    check_eq("new input index is n",MsgWaitForMultipleObjects(1,&ev,FALSE,0,QS_POSTMESSAGE),WAIT_OBJECT_0+1);
    drain();

    HANDLE t=CreateThread(NULL,0,poster,w,0,NULL);
    DWORD start=GetTickCount();
    DWORD r=MsgWaitForMultipleObjectsEx(1,&ev,5000,QS_ALLINPUT,MWMO_INPUTAVAILABLE);
    DWORD took=GetTickCount()-start;
    check_eq("post from another thread wakes the wait",r,WAIT_OBJECT_0+1);
    check("wake is prompt",took<2000);
    /* The window may also get system messages (sizing, activation), which
       can end the wait before the post arrives: keep waiting for it. */
    int got=0;
    while(!got && GetTickCount()-start<5000) {
        while(PeekMessageW(&m,w,0,0,PM_REMOVE)) if(m.message==WM_USER+7 && m.wParam==7) got=1;
        if(!got) MsgWaitForMultipleObjectsEx(0,NULL,100,QS_ALLINPUT,MWMO_INPUTAVAILABLE);
    }
    check("cross-thread post is retrieved",got);
    if(t){WaitForSingleObject(t,5000);CloseHandle(t);}
    drain();
    start=GetTickCount();
    check_eq("timeout elapses",MsgWaitForMultipleObjectsEx(0,NULL,150,QS_POSTMESSAGE,MWMO_INPUTAVAILABLE),WAIT_TIMEOUT);
    check("timeout is roughly honoured",GetTickCount()-start>=100);
    SetLastError(0);
    check_eq("64 handles are too many",MsgWaitForMultipleObjects(64,evs,FALSE,0,QS_ALLINPUT),WAIT_FAILED);
    check_eq("too many handles: ERROR_INVALID_PARAMETER",GetLastError(),ERROR_INVALID_PARAMETER);
    CloseHandle(ev);
}

static HWND worker_window;
static HANDLE worker_ready, worker_go;

/* A thread with its own window: it must receive what is posted to its
   window, and nothing posted to the main thread's. */
static DWORD WINAPI queue_owner(void *arg) {
    (void)arg;
    worker_window=make_window();
    MSG m; PeekMessageW(&m,NULL,0,0,PM_NOREMOVE); /* make sure the queue exists */
    SetEvent(worker_ready);
    WaitForSingleObject(worker_go,5000);
    DWORD start=GetTickCount(); int mine=0, foreign=0;
    while(GetTickCount()-start<1000 && !mine) {
        while(PeekMessageW(&m,NULL,0,0,PM_REMOVE)) {
            if(m.message==WM_USER+20 && m.hwnd==worker_window) mine=1;
            if(m.message==WM_USER+21) foreign=1;
        }
        MsgWaitForMultipleObjectsEx(0,NULL,50,QS_ALLINPUT,MWMO_INPUTAVAILABLE);
    }
    DestroyWindow(worker_window);
    return mine && !foreign ? 0 : (mine ? 2 : 1);
}

static void thread_queues(HWND w) {
    drain();
    worker_ready=CreateEventW(NULL,TRUE,FALSE,NULL); worker_go=CreateEventW(NULL,TRUE,FALSE,NULL);
    HANDLE t=CreateThread(NULL,0,queue_owner,NULL,0,NULL);
    check("worker window",WaitForSingleObject(worker_ready,5000)==WAIT_OBJECT_0 && worker_window);
    check("post to the worker's window",PostMessageW(worker_window,WM_USER+20,0,0));
    check("post to this thread's window",PostMessageW(w,WM_USER+21,0,0));
    SetEvent(worker_go);
    DWORD code=9;
    check("each thread gets only its own windows' messages",WaitForSingleObject(t,5000)==WAIT_OBJECT_0 && GetExitCodeThread(t,&code) && code==0);
    MSG m; int seen_worker=0, mine=0;
    while(PeekMessageW(&m,NULL,0,0,PM_REMOVE)) { if(m.message==WM_USER+20) seen_worker=1; if(m.message==WM_USER+21) mine=1; }
    check("this thread keeps its own post",mine);
    check("this thread never sees the worker's post",!seen_worker);
    CloseHandle(t); CloseHandle(worker_ready); CloseHandle(worker_go);
}

static void window_stores(HWND w, HWND w2) {
    IPropertyStore *a=NULL,*b=NULL; void *u=(void *)1; DWORD n=99;
    check_eq("NULL window: E_INVALIDARG",(DWORD)SHGetPropertyStoreForWindow(NULL,&iid_store,(void **)&a),(DWORD)E_INVALIDARG);
    check("NULL window clears the output",a==NULL);
    check_eq("NULL output: E_POINTER",(DWORD)SHGetPropertyStoreForWindow(w,&iid_store,NULL),(DWORD)E_POINTER);
    check_eq("unknown interface",(DWORD)SHGetPropertyStoreForWindow(w,&iid_bogus,&u),(DWORD)E_NOINTERFACE);
    check("unknown interface clears the output",u==NULL);
    if(SHGetPropertyStoreForWindow(w,&iid_unknown,&u)==S_OK && u) { check("IUnknown store",1); IUnknown_Release((IUnknown *)u); }
    else check("IUnknown store",0);
    if(SHGetPropertyStoreForWindow(w,&iid_store,(void **)&a)!=S_OK || !a) { check("window store",0); return; }
    check_eq("empty store count fails",(DWORD)IPropertyStore_GetCount(a,&n),(DWORD)E_FAIL);
    check_eq("empty store count",n,0);
    PROPVARIANT v; PropVariantInit(&v);
    check_eq("unset value: S_OK",(DWORD)IPropertyStore_GetValue(a,&key_appid,&v),S_OK);
    check_eq("unset value is VT_EMPTY",v.vt,VT_EMPTY);
    PROPVARIANT s; PropVariantInit(&s); s.vt=VT_LPWSTR; s.pwszVal=L"OSyx.Test";
    PROPVARIANT i; PropVariantInit(&i); i.vt=VT_UI4; i.ulVal=42;
    check_eq("set string",(DWORD)IPropertyStore_SetValue(a,&key_appid,&s),S_OK);
    check_eq("set number",(DWORD)IPropertyStore_SetValue(a,&key_other,&i),S_OK);
    check_eq("commit",(DWORD)IPropertyStore_Commit(a),S_OK);
    check("count after sets",IPropertyStore_GetCount(a,&n)==S_OK && n==2);
    PROPERTYKEY k;
    check("first key in order",IPropertyStore_GetAt(a,0,&k)==S_OK && mem_eq(&k.fmtid,&key_appid.fmtid,sizeof(GUID)) && k.pid==5);
    check_eq("key past the end",(DWORD)IPropertyStore_GetAt(a,2,&k),(DWORD)E_INVALIDARG);
    IPropertyStore_Release(a);

    check_eq("second store of the window",(DWORD)SHGetPropertyStoreForWindow(w,&iid_store,(void **)&b),S_OK);
    PropVariantInit(&v);
    check_eq("stored value: S_FALSE",(DWORD)IPropertyStore_GetValue(b,&key_appid,&v),S_FALSE);
    check("value outlives the first store",v.vt==VT_LPWSTR && v.pwszVal && wide_eq(v.pwszVal,L"OSyx.Test"));
    PropVariantClear(&v); PropVariantInit(&v);
    IPropertyStore_GetValue(b,&key_other,&v);
    check("numbers come back as text",v.vt==VT_LPWSTR && v.pwszVal && wide_eq(v.pwszVal,L"42"));
    PropVariantClear(&v);
    PROPVARIANT e; PropVariantInit(&e);
    check_eq("VT_EMPTY removes",(DWORD)IPropertyStore_SetValue(b,&key_other,&e),S_OK);
    check("count after removal",IPropertyStore_GetCount(b,&n)==S_OK && n==1);
    IPropertyStore_Release(b);

    if(SHGetPropertyStoreForWindow(w2,&iid_store,(void **)&b)==S_OK) {
        PropVariantInit(&v); IPropertyStore_GetValue(b,&key_appid,&v);
        check_eq("other window has its own values",v.vt,VT_EMPTY);
        IPropertyStore_Release(b);
    } else check("other window store",0);
    DestroyWindow(w2);
    if(SHGetPropertyStoreForWindow(w,&iid_store,(void **)&b)==S_OK) {
        PropVariantInit(&v); IPropertyStore_GetValue(b,&key_appid,&v);
        check("values survive another window's destruction",v.vt==VT_LPWSTR);
        PropVariantClear(&v); IPropertyStore_Release(b);
    }
    DestroyWindow(w);
    if(SHGetPropertyStoreForWindow(w,&iid_store,(void **)&b)==S_OK) {
        PropVariantInit(&v); IPropertyStore_GetValue(b,&key_appid,&v);
        check_eq("destroyed window has no values",v.vt,VT_EMPTY);
        IPropertyStore_Release(b);
    } else check("store of a destroyed window",0);
}

void __stdcall start(void) {
    WCHAR formatted[1024]; char ansi[1024];
    int formatted_n=wsprintfW(formatted,L"%s %d %x %I64u",L"Zen",-7,42,4294967297ULL);
    check("wsprintfW strings, integers and stack arguments",formatted_n==lstrlenW(formatted) && wide_eq(formatted,L"Zen -7 2a 4294967297"));
    formatted_n=wsprintfA(ansi,"%s %d %x %I64u","Zen",-7,42,4294967297ULL);
    check("wsprintfA strings, integers and stack arguments",formatted_n==lstrlenA(ansi) && ansi_eq(ansi,"Zen -7 2a 4294967297"));
    typedef int (WINAPI *WideFormat)(WCHAR *,const WCHAR *,const UINT_PTR *);
    WideFormat wide_format=(WideFormat)GetProcAddress(GetModuleHandleW(L"user32.dll"),"wvsprintfW");
    UINT_PTR args[2]={(UINT_PTR)L"Zen",17};
    check("wvsprintfW argument list",wide_format && wide_format(formatted,L"%s %u",args)==6 && wide_eq(formatted,L"Zen 17"));
    WNDCLASSW wc={0}; wc.lpfnWndProc=proc; wc.hInstance=GetModuleHandleW(NULL); wc.lpszClassName=L"msgwait";
    check("register class",RegisterClassW(&wc)!=0);
    HWND w=make_window(), w2=make_window();
    check("create windows",w && w2);
    HWND owned=CreateWindowExW(WS_EX_TOOLWINDOW,L"msgwait",L"owned helper",WS_POPUP,0,0,160,80,w,NULL,wc.hInstance,NULL);
    check("owned helper keeps its parent",owned && GetParent(owned)==w && GetWindow(owned,GW_OWNER)==w && GetAncestor(owned,GA_ROOTOWNER)==w);
    check("extended window styles survive creation",owned && (GetWindowLongPtrW(owned,GWL_EXSTYLE)&WS_EX_TOOLWINDOW)!=0);
    if(owned) DestroyWindow(owned);
    queue_waits(w);
    thread_queues(w);
    window_stores(w,w2);
    HWND owner=make_window();
    owned=CreateWindowExW(WS_EX_TOOLWINDOW,L"msgwait",L"owned helper",WS_POPUP,0,0,160,80,owner,NULL,wc.hInstance,NULL);
    check("create owned window for destruction",owner && owned);
    DestroyWindow(owner);
    check("destroying the owner destroys its helper",!IsWindow(owned));
    finish("msgwait");
}
