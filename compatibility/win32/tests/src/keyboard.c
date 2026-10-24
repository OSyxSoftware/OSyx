/* The US keyboard layout through MapVirtualKeyEx/ToUnicodeEx, layout
   queries, and keyboard state; also runs on Windows (the US layout is
   loaded without activating it there). */
#include "common.h"

static HKL us;

static int to_char(UINT vk, int shift, int ctrl, int caps, WCHAR *c) {
    BYTE ks[256]={0}; WCHAR b[4]={0};
    if(shift) ks[VK_SHIFT]=ks[VK_LSHIFT]=0x80;
    if(ctrl) ks[VK_CONTROL]=ks[VK_LCONTROL]=0x80;
    if(caps) ks[VK_CAPITAL]=1;
    int r=ToUnicodeEx(vk,MapVirtualKeyExW(vk,MAPVK_VK_TO_VSC,us),ks,b,4,4,us);
    *c=b[0];
    return r;
}

static void chars(void) {
    WCHAR c;
    check("a",to_char('A',0,0,0,&c)==1 && c=='a');
    check("Shift+a",to_char('A',1,0,0,&c)==1 && c=='A');
    check("Caps Lock+a",to_char('A',0,0,1,&c)==1 && c=='A');
    check("Caps Lock+Shift+a",to_char('A',1,0,1,&c)==1 && c=='a');
    check("Ctrl+a is SOH",to_char('A',0,1,0,&c)==1 && c==1);
    check("Shift+1",to_char('1',1,0,0,&c)==1 && c=='!');
    check("Caps Lock leaves digits",to_char('1',0,0,1,&c)==1 && c=='1');
    check("Ctrl+1 makes nothing",to_char('1',0,1,0,&c)==0);
    check("Ctrl+Shift+2 is NUL",to_char('2',1,1,0,&c)==1 && c==0);
    check("Ctrl+Shift+6 is RS",to_char('6',1,1,0,&c)==1 && c==0x1e);
    check("Shift+OEM_1",to_char(VK_OEM_1,1,0,0,&c)==1 && c==':');
    check("OEM_7",to_char(VK_OEM_7,0,0,0,&c)==1 && c=='\'');
    check("Ctrl+[ is ESC",to_char(VK_OEM_4,0,1,0,&c)==1 && c==0x1b);
    check("Ctrl+Backspace is DEL",to_char(VK_BACK,0,1,0,&c)==1 && c==0x7f);
    check("Ctrl+Enter is LF",to_char(VK_RETURN,0,1,0,&c)==1 && c=='\n');
    check("Enter is CR",to_char(VK_RETURN,0,0,0,&c)==1 && c=='\r');
    check("numpad 5",to_char(VK_NUMPAD5,0,0,0,&c)==1 && c=='5');
    check("Shift+numpad 5 makes nothing",to_char(VK_NUMPAD5,1,0,0,&c)==0);
    check("F1 makes nothing",to_char(VK_F1,0,0,0,&c)==0);
    check("left arrow makes nothing",to_char(VK_LEFT,0,0,0,&c)==0);
    BYTE ks[256]={0}; WCHAR b[2];
    ks[VK_CONTROL]=ks[VK_MENU]=0x80;
    check("AltGr+e makes nothing on US",ToUnicodeEx('E',0x12,ks,b,2,4,us)==0);
    ks[VK_CONTROL]=ks[VK_MENU]=0;
    check("key release makes nothing",ToUnicodeEx('A',0x1e|0x8000,ks,b,2,4,us)==0);
}

static void mapping(void) {
    check_eq("A to scan code",MapVirtualKeyExW('A',MAPVK_VK_TO_VSC,us),0x1e);
    check_eq("scan code to A",MapVirtualKeyExW(0x1e,MAPVK_VSC_TO_VK,us),'A');
    check_eq("A to character",MapVirtualKeyExW('A',MAPVK_VK_TO_CHAR,us),'A');
    check_eq("OEM_2 to character",MapVirtualKeyExW(VK_OEM_2,MAPVK_VK_TO_CHAR,us),'/');
    check_eq("left arrow scan code",MapVirtualKeyExW(VK_LEFT,MAPVK_VK_TO_VSC,us),0x4b);
    check_eq("left arrow has no E0 prefix in the map",MapVirtualKeyExW(VK_LEFT,MAPVK_VK_TO_VSC_EX,us),0x4b);
    check_eq("numpad divide extended scan code",MapVirtualKeyExW(VK_DIVIDE,MAPVK_VK_TO_VSC_EX,us),0xe035);
    check_eq("right Ctrl extended scan code",MapVirtualKeyExW(VK_RCONTROL,MAPVK_VK_TO_VSC_EX,us),0xe01d);
    check_eq("Shift scan code",MapVirtualKeyExW(VK_SHIFT,MAPVK_VK_TO_VSC,us),0x2a);
    check_eq("right Shift scan code",MapVirtualKeyExW(VK_RSHIFT,MAPVK_VK_TO_VSC,us),0x36);
    check_eq("scan 0x36 to Shift",MapVirtualKeyExW(0x36,MAPVK_VSC_TO_VK,us),VK_SHIFT);
    check_eq("scan 0x36 to right Shift",MapVirtualKeyExW(0x36,MAPVK_VSC_TO_VK_EX,us),VK_RSHIFT);
    check_eq("scan 0x1d to left Ctrl",MapVirtualKeyExW(0x1d,MAPVK_VSC_TO_VK_EX,us),VK_LCONTROL);
    check_eq("scan 0xe01d to right Ctrl",MapVirtualKeyExW(0xe01d,MAPVK_VSC_TO_VK_EX,us),VK_RCONTROL);
    check_eq("scan 0xe038 to right Alt",MapVirtualKeyExW(0xe038,MAPVK_VSC_TO_VK_EX,us),VK_RMENU);
    check_eq("scan 0x4b to left arrow",MapVirtualKeyExW(0x4b,MAPVK_VSC_TO_VK,us),VK_LEFT);
    check_eq("F12 scan code",MapVirtualKeyExW(VK_F12,MAPVK_VK_TO_VSC,us),0x58);
    check_eq("unmapped key",MapVirtualKeyExW(0xff,MAPVK_VK_TO_VSC,us),0);
    check_eq("unknown map type",MapVirtualKeyExW('A',99,us),0);
}

static void layouts(void) {
    HKL cur=GetKeyboardLayout(0), list[8];
    check("a current layout",cur!=0);
    int n=GetKeyboardLayoutList(0,NULL), m=GetKeyboardLayoutList(8,list), seen=0;
    check("layout count",n>=1 && m==n);
    for(int i=0;i<m;i++) if(list[i]==cur) seen=1;
    check("current layout is listed",seen);
    WCHAR name[KL_NAMELENGTH]={0};
    check("layout name",GetKeyboardLayoutNameW(name) && name[8]==0 && name[0]=='0');
    char aname[KL_NAMELENGTH]={0};
    check("ANSI layout name",GetKeyboardLayoutNameA(aname) && aname[8]==0);
    check("US layout handle",(UINT_PTR)us==0x04090409);
}

static void state(void) {
    BYTE ks[256], back[256];
    for(int i=0;i<256;i++) ks[i]=0;
    ks['Q']=0x80; ks[VK_CAPITAL]=1;
    check("set keyboard state",SetKeyboardState(ks));
    check("get keyboard state",GetKeyboardState(back));
    check("toggle reads back",(back[VK_CAPITAL]&1)!=0);
    check("GetKeyState sees the toggle",(GetKeyState(VK_CAPITAL)&1)!=0);
    ks['Q']=0; ks[VK_CAPITAL]=0;
    SetKeyboardState(ks);
    check("release reads back",GetKeyState('Q')>=0 && (GetKeyState(VK_CAPITAL)&1)==0);
    SetLastError(0);
    check("NULL state",!GetKeyboardState(NULL) && GetLastError()==ERROR_NOACCESS);
}

void __stdcall start(void) {
    us=LoadKeyboardLayoutA("00000409",KLF_NOTELLSHELL);
    check("load the US layout",us!=0);
    chars();
    mapping();
    layouts();
    state();
    finish("keyboard");
}
