/* dwmapi window attributes and frame timing, imm32 without an input
   context, wtsapi32 session notifications; also runs on Windows. */
#include "common.h"
#include <dwmapi.h>
#include <imm.h>
#include <wtsapi32.h>
_Static_assert(sizeof(DWM_TIMING_INFO)==292 && sizeof(MARGINS)==16,"dwmapi ABI");

static LRESULT CALLBACK proc(HWND w, UINT m, WPARAM wp, LPARAM lp) { return DefWindowProcW(w,m,wp,lp); }

static void attributes(HWND w) {
    BOOL on=TRUE; DWORD d=7; RECT r={0}, wr={0};
    check_eq("set on a non-window: E_HANDLE",(DWORD)DwmSetWindowAttribute((HWND)0x1234,DWMWA_CLOAK,&on,4),(DWORD)E_HANDLE);
    check_eq("set on NULL: E_HANDLE",(DWORD)DwmSetWindowAttribute(NULL,DWMWA_CLOAK,&on,4),(DWORD)E_HANDLE);
    check_eq("set with a short value",(DWORD)DwmSetWindowAttribute(w,DWMWA_CLOAK,&on,2),(DWORD)E_INVALIDARG);
    check_eq("set without a value",(DWORD)DwmSetWindowAttribute(w,DWMWA_CLOAK,NULL,4),(DWORD)E_INVALIDARG);
    check_eq("set an unknown attribute",(DWORD)DwmSetWindowAttribute(w,999,&d,4),(DWORD)E_INVALIDARG);
    check_eq("cloak",(DWORD)DwmSetWindowAttribute(w,DWMWA_CLOAK,&on,4),S_OK);
    check("cloaked by the app",DwmGetWindowAttribute(w,DWMWA_CLOAKED,&d,4)==S_OK && d==DWM_CLOAKED_APP);
    on=FALSE; DwmSetWindowAttribute(w,DWMWA_CLOAK,&on,4);
    check("uncloaked",DwmGetWindowAttribute(w,DWMWA_CLOAKED,&d,4)==S_OK && d==0);
    check_eq("CLOAK cannot be read",(DWORD)DwmGetWindowAttribute(w,DWMWA_CLOAK,&d,4),(DWORD)E_INVALIDARG);
    check_eq("CLOAKED needs a DWORD",(DWORD)DwmGetWindowAttribute(w,DWMWA_CLOAKED,&d,2),(DWORD)E_INVALIDARG);
    check_eq("get on a non-window: E_HANDLE",(DWORD)DwmGetWindowAttribute((HWND)0x1234,DWMWA_CLOAKED,&d,4),(DWORD)E_HANDLE);
    check_eq("get an unknown attribute",(DWORD)DwmGetWindowAttribute(w,999,&d,4),(DWORD)E_INVALIDARG);
    check("non-client rendering is on",DwmGetWindowAttribute(w,DWMWA_NCRENDERING_ENABLED,&d,4)==S_OK && d==1);
    GetWindowRect(w,&wr);
    check_eq("extended frame bounds",(DWORD)DwmGetWindowAttribute(w,DWMWA_EXTENDED_FRAME_BOUNDS,&r,sizeof r),S_OK);
    check("frame lies within the window rectangle",r.left>=wr.left && r.top>=wr.top && r.right<=wr.right && r.bottom<=wr.bottom && r.right>r.left && r.bottom>r.top);
    d=1; check_eq("dark mode",(DWORD)DwmSetWindowAttribute(w,DWMWA_USE_IMMERSIVE_DARK_MODE,&d,4),S_OK);
    d=2; check_eq("corner preference",(DWORD)DwmSetWindowAttribute(w,33,&d,4),S_OK);
    d=DWMNCRP_DISABLED; check_eq("non-client rendering policy",(DWORD)DwmSetWindowAttribute(w,DWMWA_NCRENDERING_POLICY,&d,4),S_OK);
    MARGINS m={-1,-1,-1,-1};
    check_eq("extend the frame",(DWORD)DwmExtendFrameIntoClientArea(w,&m),S_OK);
    check_eq("extend a non-window's frame",(DWORD)DwmExtendFrameIntoClientArea((HWND)0x1234,&m),(DWORD)E_INVALIDARG);
    BOOL en=FALSE;
    check("composition is enabled",DwmIsCompositionEnabled(&en)==S_OK && en);
}

static void timing(HWND w) {
    DWM_TIMING_INFO a, b;
    for(DWORD i=0;i<sizeof a;i++){((char *)&a)[i]=0;((char *)&b)[i]=0;}
    a.cbSize=b.cbSize=sizeof a;
    check_eq("timing info",(DWORD)DwmGetCompositionTimingInfo(NULL,&a),S_OK);
    check("refresh rate is sensible",a.rateRefresh.uiDenominator && a.rateRefresh.uiNumerator/a.rateRefresh.uiDenominator>=20 && a.rateRefresh.uiNumerator/a.rateRefresh.uiDenominator<=500);
    LARGE_INTEGER f; QueryPerformanceFrequency(&f);
    check("refresh period matches the rate",a.qpcRefreshPeriod>0 && a.qpcRefreshPeriod<(QPC_TIME)f.QuadPart/10);
    check("vertical blank is in the past",a.qpcVBlank>0);
    check_eq("DwmFlush",(DWORD)DwmFlush(),S_OK);
    DwmFlush();
    DwmGetCompositionTimingInfo(NULL,&b);
    check("refresh count advances",b.cRefresh>a.cRefresh && b.qpcVBlank>a.qpcVBlank);
    a.cbSize=8;
    check("wrong structure size fails",FAILED(DwmGetCompositionTimingInfo(NULL,&a)));
    a.cbSize=sizeof a;
    check_eq("per-window timing is not supported",(DWORD)DwmGetCompositionTimingInfo(w,&a),(DWORD)E_INVALIDARG);
}

static void ime(HWND w) {
    HIMC c=ImmGetContext(w);
    check("release the input context",ImmReleaseContext(w,c));
    check("associate the default context",ImmAssociateContextEx(w,NULL,IACE_DEFAULT));
    check("NULL context is closed",!ImmGetOpenStatus(NULL));
    check_eq("NULL context has no composition",(DWORD)ImmGetCompositionStringW(NULL,GCS_COMPSTR,NULL,0),0);
    SetLastError(0);
    check("session notification",WTSRegisterSessionNotification(w,NOTIFY_FOR_THIS_SESSION));
    check("session notification removed",WTSUnRegisterSessionNotification(w));
    SetLastError(0);
    check("session notification for a non-window fails",!WTSRegisterSessionNotification((HWND)0x1234,0) && GetLastError()==ERROR_INVALID_PARAMETER);
}

void __stdcall start(void) {
    WNDCLASSW wc={0}; wc.lpfnWndProc=proc; wc.hInstance=GetModuleHandleW(NULL); wc.lpszClassName=L"dwm";
    RegisterClassW(&wc);
    HWND w=CreateWindowExW(0,L"dwm",L"dwm",WS_OVERLAPPEDWINDOW,10,20,300,200,NULL,NULL,wc.hInstance,NULL);
    check("create window",w!=NULL);
    attributes(w);
    timing(w);
    ime(w);
    DestroyWindow(w);
    finish("dwm");
}
