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

static HWND frame_window;
static unsigned frame_sequence, frame_changing, frame_changed, frame_size;
static DWORD frame_thread;
static int frame_mutate, frame_consume;

static LRESULT CALLBACK proc(HWND w, UINT m, WPARAM wp, LPARAM lp) {
    if(w==frame_window) {
        if(m==WM_WINDOWPOSCHANGING) {
            frame_changing=++frame_sequence;
            frame_thread=GetCurrentThreadId();
            if(frame_mutate) { WINDOWPOS *p=(WINDOWPOS *)lp; p->cx=380; p->cy=280; }
        }
        if(m==WM_WINDOWPOSCHANGED) {
            frame_changed=++frame_sequence;
            if(frame_consume) return 0;
        }
        if(m==WM_SIZE) frame_size=++frame_sequence;
    }
    return DefWindowProcW(w,m,wp,lp);
}

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

static void filtered_messages(HWND w) {
    drain();
    MSG m={0}, first={0};
    PostMessageW(w,WM_USER+40,40,0);
    PostMessageW(NULL,WM_USER+41,41,0);
    PostMessageW(w,WM_USER+42,42,0);
    check("range filter skips earlier unrelated messages",PeekMessageW(&m,NULL,WM_USER+42,WM_USER+42,PM_REMOVE) && m.message==WM_USER+42);
    check("paint-only peek leaves posted work queued",!PeekMessageW(&m,NULL,0,0,PM_REMOVE|PM_QS_PAINT));
    check("thread-only filter skips window messages",PeekMessageW(&m,(HWND)-1,0,0,PM_REMOVE) && !m.hwnd && m.message==WM_USER+41);
    check("PM_NOREMOVE observes first remaining message",PeekMessageW(&first,w,0,0,PM_NOREMOVE) && first.message==WM_USER+40);
    check("PM_NOREMOVE preserves order and content",PeekMessageW(&m,w,0,0,PM_REMOVE) && m.message==first.message && m.wParam==first.wParam && m.hwnd==first.hwnd);
    check("filtered retrieval empties only matching posts",!PeekMessageW(&m,NULL,WM_USER+40,WM_USER+42,PM_REMOVE));
    PostMessageW(w,WM_USER+43,43,0);
    PostMessageW(w,WM_USER+44,44,0);
    check("GetMessage honours the range",GetMessageW(&m,w,WM_USER+44,WM_USER+44)>0 && m.message==WM_USER+44);
    check("GetMessage leaves earlier unrelated work",PeekMessageW(&m,w,WM_USER+43,WM_USER+43,PM_REMOVE) && m.message==WM_USER+43);
    UINT_PTR timer=SetTimer(w,91,20,NULL);
    Sleep(60);
    check("paint-only peek does not generate timers",!PeekMessageW(&m,w,0,0,PM_REMOVE|PM_QS_PAINT));
    check("timer peek generates without removing",PeekMessageW(&first,w,WM_TIMER,WM_TIMER,PM_NOREMOVE) && first.message==WM_TIMER && first.wParam==timer);
    KillTimer(w,timer);
    check("generated timer survives KillTimer and repeated peek",PeekMessageW(&m,w,WM_TIMER,WM_TIMER,PM_NOREMOVE) && m.message==first.message && m.wParam==first.wParam && m.time==first.time);
    check("generated timer is removed once",PeekMessageW(&m,w,WM_TIMER,WM_TIMER,PM_REMOVE) && m.wParam==timer && !PeekMessageW(&m,w,WM_TIMER,WM_TIMER,PM_REMOVE));
    PostQuitMessage(23);
    check("WM_QUIT bypasses range and window filters",PeekMessageW(&first,(HWND)-1,WM_KEYFIRST,WM_KEYLAST,PM_NOREMOVE) && first.message==WM_QUIT && first.wParam==23);
    check("WM_QUIT survives PM_NOREMOVE",PeekMessageW(&m,w,WM_TIMER,WM_TIMER,PM_REMOVE) && m.message==WM_QUIT && m.wParam==23);
    drain();
}

static void window_states(HWND w) {
    typedef BOOL (WINAPI *Arranged)(HWND);
    Arranged arranged=(Arranged)GetProcAddress(GetModuleHandleW(L"user32.dll"),"IsWindowArranged");
    check("window arrangement query is exported",arranged!=NULL);
    SetLastError(1234);
    check("a normal unsnapped window is not arranged",arranged && !arranged(w) && GetLastError()==1234);
    SetLastError(1234);
    check("valid zero window ID preserves last error",GetWindowLongPtrW(w,GWLP_ID)==0 && GetLastError()==1234);
    SetWindowPos(w,NULL,0,0,320,240,SWP_NOMOVE|SWP_NOZORDER|SWP_NOACTIVATE);
    HRGN region=CreateRectRgn(2,3,9,11);
    RECT bounds={0};
    check("normal window has no explicit region",GetWindowRgn(w,region)==ERROR);
    check("default window region query preserves destination",GetRgnBox(region,&bounds)==SIMPLEREGION && bounds.left==2 && bounds.top==3 && bounds.right==9 && bounds.bottom==11);
    DeleteObject(region);
    check("hidden window is not minimized",!IsIconic(w) && !IsZoomed(w) && !IsWindowVisible(w));
    WINDOWPLACEMENT place={0}; place.length=sizeof(place);
    check("hidden normal placement is not a minimized placement",GetWindowPlacement(w,&place) && place.showCmd==SW_SHOWNORMAL);
    ShowWindow(w,SW_SHOWNORMAL);
    check("shown window is visible and not minimized",IsWindowVisible(w) && !IsIconic(w));
    check("ShowWindow sets WS_VISIBLE",(GetWindowLongPtrW(w,GWL_STYLE)&WS_VISIBLE)!=0);
    UpdateWindow(w);
    ValidateRect(w,NULL);
    RECT dirty={3,4,13,14}, update={9,9,9,9};
    check("validated window has no update rectangle",!GetUpdateRect(w,&update,FALSE) && IsRectEmpty(&update));
    InvalidateRect(w,&dirty,FALSE);
    check("GetUpdateRect reports the invalidated rectangle",GetUpdateRect(w,&update,FALSE) && update.left==3 && update.top==4 && update.right==13 && update.bottom==14);
    check("querying update state does not validate it",GetUpdateRect(w,NULL,FALSE));
    ValidateRect(w,NULL);
    check("validation clears update state",!GetUpdateRect(w,NULL,FALSE));
    ShowWindow(w,SW_MINIMIZE);
    check("minimize updates IsIconic immediately",IsIconic(w) && !IsZoomed(w));
    check("a minimized window is not arranged",arranged && !arranged(w));
    check("minimize sets WS_MINIMIZE",(GetWindowLongPtrW(w,GWL_STYLE)&WS_MINIMIZE)!=0);
    check("minimized window remains visible",IsWindowVisible(w));
    check("placement reports minimized state",GetWindowPlacement(w,&place) && place.showCmd==SW_SHOWMINIMIZED);
    ShowWindow(w,SW_RESTORE);
    check("restore clears minimized state",!IsIconic(w) && !(GetWindowLongPtrW(w,GWL_STYLE)&WS_MINIMIZE));
    ShowWindow(w,SW_MAXIMIZE);
    check("maximize sets zoomed state",IsZoomed(w) && !IsIconic(w) && (GetWindowLongPtrW(w,GWL_STYLE)&WS_MAXIMIZE));
    check("maximized and arranged are distinct states",arranged && !arranged(w));
    check("placement reports maximized state",GetWindowPlacement(w,&place) && place.showCmd==SW_SHOWMAXIMIZED);
    LONG normal_width=place.rcNormalPosition.right-place.rcNormalPosition.left;
    check("maximizing preserves the normal placement width",normal_width==320);
    ShowWindow(w,SW_MINIMIZE);
    ShowWindow(w,SW_RESTORE);
    check("restore of a minimized maximized window returns to maximized",IsZoomed(w) && !IsIconic(w));
    Sleep(50);
    MSG state_msg;
    int stale_size=0;
    while(PeekMessageW(&state_msg,w,0,0,PM_REMOVE)) {
        if(state_msg.message==WM_SIZE && state_msg.wParam!=SIZE_MAXIMIZED) stale_size=1;
        DispatchMessageW(&state_msg);
    }
    check("old restored-size messages do not follow a synchronous maximize",!stale_size);
    check("queued compositor acknowledgements preserve the final requested state",IsZoomed(w) && !IsIconic(w));
    ShowWindow(w,SW_SHOWNORMAL);
    ShowWindow(w,SW_HIDE);
    check("hide clears visibility without minimizing",!IsWindowVisible(w) && !IsIconic(w) && !(GetWindowLongPtrW(w,GWL_STYLE)&WS_VISIBLE));
    SetWindowPos(w,NULL,0,0,0,0,SWP_NOMOVE|SWP_NOSIZE|SWP_NOZORDER|SWP_NOACTIVATE|SWP_SHOWWINDOW);
    check("SWP_SHOWWINDOW sets visibility and style",IsWindowVisible(w) && (GetWindowLongPtrW(w,GWL_STYLE)&WS_VISIBLE));
    SetWindowPos(w,NULL,0,0,0,0,SWP_NOMOVE|SWP_NOSIZE|SWP_NOZORDER|SWP_NOACTIVATE|SWP_HIDEWINDOW);
    check("SWP_HIDEWINDOW clears visibility and style",!IsWindowVisible(w) && !(GetWindowLongPtrW(w,GWL_STYLE)&WS_VISIBLE));
    place.showCmd=SW_SHOWNORMAL;
    place.rcNormalPosition=(RECT){34,56,394,316};
    check("SetWindowPlacement applies normal geometry and show state",SetWindowPlacement(w,&place) && IsWindowVisible(w) && !IsIconic(w) && GetWindowPlacement(w,&place) && place.rcNormalPosition.right-place.rcNormalPosition.left==360);
    ShowWindow(w,SW_HIDE);
    drain();
}

static void window_frame_callbacks(HWND w) {
    frame_window=w;
    frame_sequence=frame_changing=frame_changed=frame_size=0;
    frame_mutate=1;
    check("SetWindowPos accepts a changing callback",SetWindowPos(w,NULL,0,0,350,250,SWP_NOMOVE|SWP_NOZORDER|SWP_NOACTIVATE));
    check("frame callbacks precede the derived WM_SIZE",frame_changing && frame_changing<frame_changed && frame_changed<frame_size);
    RECT r={0}; GetWindowRect(w,&r);
    check("WM_WINDOWPOSCHANGING can alter the requested size",r.right-r.left==380 && r.bottom-r.top==280);
    check("frame callback runs on the window thread",frame_thread==GetCurrentThreadId());
    frame_mutate=0;
    frame_sequence=frame_changing=frame_changed=frame_size=0;
    SetWindowPos(w,NULL,0,0,390,290,SWP_NOMOVE|SWP_NOZORDER|SWP_NOACTIVATE|SWP_NOSENDCHANGING);
    check("SWP_NOSENDCHANGING skips only the changing callback",!frame_changing && frame_changed && frame_changed<frame_size);
    frame_consume=1;
    frame_sequence=frame_changing=frame_changed=frame_size=0;
    SetWindowPos(w,NULL,0,0,400,300,SWP_NOMOVE|SWP_NOZORDER|SWP_NOACTIVATE);
    check("consuming WM_WINDOWPOSCHANGED suppresses derived WM_SIZE",frame_changing && frame_changed && !frame_size);
    frame_consume=0;
    frame_sequence=frame_changing=frame_changed=frame_size=0;
    ShowWindow(w,SW_MAXIMIZE);
    WINDOWPLACEMENT p={0}; p.length=sizeof(p);
    check("ShowWindow sends frame callbacks with its new state",frame_changing && frame_changed && GetWindowPlacement(w,&p) && p.showCmd==SW_SHOWMAXIMIZED);
    ShowWindow(w,SW_HIDE);
    frame_window=NULL;
    drain();
}

static void system_fonts(void) {
    _Static_assert(sizeof(NONCLIENTMETRICSW)==504 && sizeof(NONCLIENTMETRICSA)==344,"nonclient metrics layouts");
    NONCLIENTMETRICSW ncm={0}; ncm.cbSize=sizeof(ncm);
    check("wide nonclient metrics query",SystemParametersInfoW(SPI_GETNONCLIENTMETRICS,sizeof(ncm),&ncm,0));
    check("nonclient metrics include usable UI dimensions",ncm.iScrollWidth>0 && ncm.iCaptionHeight>0 && ncm.iMenuHeight>0);
    check("nonclient metrics include five usable UI fonts",ncm.lfCaptionFont.lfHeight!=0 && ncm.lfSmCaptionFont.lfHeight!=0 && ncm.lfMenuFont.lfHeight!=0 && ncm.lfStatusFont.lfHeight!=0 && ncm.lfMessageFont.lfHeight!=0 && ncm.lfMessageFont.lfFaceName[0]);
    NONCLIENTMETRICSA ansi={0}; ansi.cbSize=sizeof(ansi);
    check("ANSI nonclient metrics use the ANSI layout",SystemParametersInfoA(SPI_GETNONCLIENTMETRICS,sizeof(ansi),&ansi,0) && ansi.lfMessageFont.lfHeight!=0 && ansi.lfMessageFont.lfFaceName[0] && ansi.iMenuHeight>0);
    ncm.cbSize=sizeof(ncm)-4; ncm.iPaddedBorderWidth=0x12345678;
    check("legacy nonclient metrics query",SystemParametersInfoW(SPI_GETNONCLIENTMETRICS,ncm.cbSize,&ncm,0));
    check("legacy metrics preserve memory beyond cbSize",ncm.iPaddedBorderWidth==0x12345678);
    ncm.cbSize=4;
    check("undersized nonclient metrics are rejected",!SystemParametersInfoW(SPI_GETNONCLIENTMETRICS,4,&ncm,0));
    LOGFONTW icon={0}; LOGFONTA icon_a={0};
    check("wide icon-title font",SystemParametersInfoW(SPI_GETICONTITLELOGFONT,sizeof(icon),&icon,0) && icon.lfHeight!=0 && icon.lfFaceName[0]);
    check("ANSI icon-title font",SystemParametersInfoA(SPI_GETICONTITLELOGFONT,sizeof(icon_a),&icon_a,0) && icon_a.lfHeight!=0 && icon_a.lfFaceName[0]);
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
    DWORD worker_tid=0;
    HANDLE t=CreateThread(NULL,0,queue_owner,NULL,0,&worker_tid);
    check("worker window",WaitForSingleObject(worker_ready,5000)==WAIT_OBJECT_0 && worker_window);
    DWORD window_pid=0;
    check("window thread query returns its creator, not caller",GetWindowThreadProcessId(worker_window,&window_pid)==worker_tid && window_pid==GetCurrentProcessId());
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
    system_fonts();
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
    filtered_messages(w);
    thread_queues(w);
    window_stores(w,w2);
    HWND state_window=make_window();
    window_states(state_window);
    window_frame_callbacks(state_window);
    DestroyWindow(state_window);
    HWND owner=make_window();
    owned=CreateWindowExW(WS_EX_TOOLWINDOW,L"msgwait",L"owned helper",WS_POPUP,0,0,160,80,owner,NULL,wc.hInstance,NULL);
    check("create owned window for destruction",owner && owned);
    DestroyWindow(owner);
    check("destroying the owner destroys its helper",!IsWindow(owned));
    finish("msgwait");
}
