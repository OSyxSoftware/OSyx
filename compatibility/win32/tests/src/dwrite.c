/* DirectWrite: font files, faces, metrics, glyph indices and tables,
   glyph-run analysis, the system collection and GDI interop; also runs on
   Windows, where the expected values were taken from DirectWrite itself.
   The font is OSyx's Lato Regular (graphics/assets/fonts/windows), at
   C:\Windows\Fonts on OSyx or named by OSYX_TEST_FONT elsewhere. */
#define COBJMACROS
#include "common.h"
#include <dwrite.h>

static const GUID iid_factory={0xb859ee5a,0xd838,0x4b5b,{0xa2,0xe8,0x1a,0xdc,0x7d,0x93,0xdb,0x48}};
static const GUID iid_local_loader={0xb2d9f3ec,0xc9fe,0x4a11,{0xa2,0xec,0xd8,0x62,0x08,0xf7,0xc0,0xa2}};

static int wide_eq(const WCHAR *a, const WCHAR *b) { while(*a && *a==*b){a++;b++;} return *a==*b; }

static IDWriteFontFace *face_from_file(IDWriteFactory *f, const WCHAR *path) {
    IDWriteFontFile *file=NULL; IDWriteFontFace *face=NULL;
    check("font file reference",IDWriteFactory_CreateFontFileReference(f,path,NULL,&file)==S_OK && file);
    if(!file) return NULL;
    BOOL supported=FALSE; DWRITE_FONT_FILE_TYPE ft; DWRITE_FONT_FACE_TYPE kind; UINT32 faces=0;
    check("analyze the file",IDWriteFontFile_Analyze(file,&supported,&ft,&kind,&faces)==S_OK && supported && ft==DWRITE_FONT_FILE_TYPE_TRUETYPE && kind==DWRITE_FONT_FACE_TYPE_TRUETYPE && faces==1);
    const void *key=NULL; UINT32 key_size=0; IDWriteFontFileLoader *loader=NULL;
    check("reference key",IDWriteFontFile_GetReferenceKey(file,&key,&key_size)==S_OK && key && key_size>8);
    check("the local loader",IDWriteFontFile_GetLoader(file,&loader)==S_OK && loader);
    if(loader) {
        IDWriteLocalFontFileLoader *local=NULL;
        check("it is a local loader",IDWriteFontFileLoader_QueryInterface(loader,&iid_local_loader,(void **)&local)==S_OK && local);
        if(local) {
            WCHAR back[MAX_PATH]; UINT32 len=0;
            check("path from the key",IDWriteLocalFontFileLoader_GetFilePathLengthFromKey(local,key,key_size,&len)==S_OK && len>0 && len<MAX_PATH &&
                  IDWriteLocalFontFileLoader_GetFilePathFromKey(local,key,key_size,back,MAX_PATH)==S_OK && back[len]==0);
            IDWriteFontFileStream *stream=NULL; UINT64 size=0;
            check("stream from the key",IDWriteLocalFontFileLoader_CreateStreamFromKey(local,key,key_size,&stream)==S_OK && stream &&
                  IDWriteFontFileStream_GetFileSize(stream,&size)==S_OK && size==607720);
            if(stream) {
                const void *frag=NULL; void *ctx=NULL;
                check("read a fragment",IDWriteFontFileStream_ReadFileFragment(stream,&frag,0,4,&ctx)==S_OK && frag && ((const BYTE *)frag)[1]==1);
                IDWriteFontFileStream_ReleaseFileFragment(stream,ctx);
                check("no fragment past the end",IDWriteFontFileStream_ReadFileFragment(stream,&frag,size-2,4,&ctx)!=S_OK);
                IDWriteFontFileStream_Release(stream);
            }
            IDWriteLocalFontFileLoader_Release(local);
        }
        IDWriteFontFileLoader_Release(loader);
    }
    check("font face",IDWriteFactory_CreateFontFace(f,DWRITE_FONT_FACE_TYPE_TRUETYPE,1,&file,0,DWRITE_FONT_SIMULATIONS_NONE,&face)==S_OK && face);
    IDWriteFontFile_Release(file);
    return face;
}

static void face_checks(IDWriteFontFace *face) {
    check("face type",IDWriteFontFace_GetType(face)==DWRITE_FONT_FACE_TYPE_TRUETYPE);
    check("face index",IDWriteFontFace_GetIndex(face)==0);
    check("not a symbol font",!IDWriteFontFace_IsSymbolFont(face));
    check_eq("glyph count",IDWriteFontFace_GetGlyphCount(face),3023);
    DWRITE_FONT_METRICS m;
    IDWriteFontFace_GetMetrics(face,&m);
    check_eq("units per em",m.designUnitsPerEm,2000);
    check_eq("ascent",m.ascent,1610);
    check_eq("descent",m.descent,390);
    check_eq("line gap",(DWORD)(INT16)m.lineGap,400);
    check_eq("cap height",m.capHeight,1433);
    check_eq("x height",m.xHeight,1013);
    check_eq("underline position",(DWORD)(INT16)m.underlinePosition,(DWORD)(INT16)-466);
    check_eq("underline thickness",m.underlineThickness,89);
    check_eq("strikethrough position",(DWORD)(INT16)m.strikethroughPosition,550);
    check_eq("strikethrough thickness",m.strikethroughThickness,89);
    UINT32 cps[4]={'A','a',' ',0x10FFFF}; UINT16 g[4]={9,9,9,9};
    check("glyph indices",IDWriteFontFace_GetGlyphIndices(face,cps,4,g)==S_OK);
    check("A, a and space have glyphs",g[0] && g[1] && g[2] && g[0]!=g[1]);
    check_eq("an unmapped character",g[3],0);
    DWRITE_GLYPH_METRICS gm[3];
    check("design glyph metrics",IDWriteFontFace_GetDesignGlyphMetrics(face,g,3,gm,FALSE)==S_OK);
    check_eq("A advance",gm[0].advanceWidth,1354);
    check_eq("A left side bearing",(DWORD)gm[0].leftSideBearing,6);
    check_eq("A right side bearing",(DWORD)gm[0].rightSideBearing,6);
    check_eq("A vertical origin",(DWORD)gm[0].verticalOriginY,1610);
    check_eq("A top side bearing",(DWORD)gm[0].topSideBearing,177);
    check_eq("A bottom side bearing",(DWORD)gm[0].bottomSideBearing,390);
    check_eq("A advance height",gm[0].advanceHeight,2000);
    check_eq("space advance",gm[2].advanceWidth,512);
    check_eq("space right side bearing",(DWORD)gm[2].rightSideBearing,512);
    check_eq("space top side bearing",(DWORD)gm[2].topSideBearing,1610);
    check_eq("space bottom side bearing",(DWORD)gm[2].bottomSideBearing,390);
    const void *table=NULL; UINT32 size=0; void *ctx=NULL; BOOL exists=FALSE;
    check("head table",IDWriteFontFace_TryGetFontTable(face,DWRITE_MAKE_OPENTYPE_TAG('h','e','a','d'),&table,&size,&ctx,&exists)==S_OK && exists && size==54 && table);
    if(exists) IDWriteFontFace_ReleaseFontTable(face,ctx);
    check("missing table",IDWriteFontFace_TryGetFontTable(face,DWRITE_MAKE_OPENTYPE_TAG('z','z','z','z'),&table,&size,&ctx,&exists)==S_OK && !exists && size==0);
}

static void analysis_checks(IDWriteFactory *f, IDWriteFontFace *face) {
    UINT32 cp='H'; UINT16 g=0; IDWriteFontFace_GetGlyphIndices(face,&cp,1,&g);
    FLOAT adv=10.0f; DWRITE_GLYPH_OFFSET off={0,0};
    DWRITE_GLYPH_RUN run={face,20.0f,1,&g,&adv,&off,FALSE,0};
    IDWriteGlyphRunAnalysis *a=NULL;
    check("glyph run analysis",IDWriteFactory_CreateGlyphRunAnalysis(f,&run,1.0f,NULL,DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC,DWRITE_MEASURING_MODE_NATURAL,10.0f,30.0f,&a)==S_OK && a);
    if(!a) return;
    RECT r={0}, empty={1,1,1,1};
    check("ClearType bounds",IDWriteGlyphRunAnalysis_GetAlphaTextureBounds(a,DWRITE_TEXTURE_CLEARTYPE_3x1,&r)==S_OK);
    /* H at 20 px: about 11 px wide and 14 px tall, standing on y = 30. */
    check("H lies right of the origin",r.left>=10 && r.left<=13 && r.right>=19 && r.right<=24);
    check("H stands on the baseline",r.bottom>=29 && r.bottom<=31 && r.top>=14 && r.top<=17);
    check("no aliased texture in an antialiased mode",IDWriteGlyphRunAnalysis_GetAlphaTextureBounds(a,DWRITE_TEXTURE_ALIASED_1x1,&empty)==S_OK && empty.left==empty.right);
    int w=r.right-r.left, h=r.bottom-r.top;
    static BYTE tex[3*64*64];
    if(w>0 && h>0 && w*h*3<=(int)sizeof tex) {
        check("too small a buffer",IDWriteGlyphRunAnalysis_CreateAlphaTexture(a,DWRITE_TEXTURE_CLEARTYPE_3x1,&r,tex,3)!=S_OK);
        check("ClearType texture",IDWriteGlyphRunAnalysis_CreateAlphaTexture(a,DWRITE_TEXTURE_CLEARTYPE_3x1,&r,tex,(UINT32)(w*h*3))==S_OK);
        int full=0, blank=0;
        for(int i=0;i<w*h*3;i++){ if(tex[i]>=250) full++; if(tex[i]==0) blank++; }
        check("the stems are solid",full>=3*2*10);
        check("the counter is empty",blank>=3*5*5);
        /* The middle of the left stem, two pixels in from the left edge. */
        int y=h/4, x=1;
        check("left stem is covered",tex[(y*w+x)*3+1]>=200);
        check("between the stems at the top is empty",tex[(1*w+w/2)*3+1]<=40);
    } else check("texture size",0);
    IDWriteGlyphRunAnalysis_Release(a);
    a=NULL;
    check("aliased analysis",IDWriteFactory_CreateGlyphRunAnalysis(f,&run,1.0f,NULL,DWRITE_RENDERING_MODE_ALIASED,DWRITE_MEASURING_MODE_NATURAL,0.0f,0.0f,&a)==S_OK && a);
    if(a) {
        check("aliased bounds",IDWriteGlyphRunAnalysis_GetAlphaTextureBounds(a,DWRITE_TEXTURE_ALIASED_1x1,&r)==S_OK && r.right>r.left);
        IDWriteGlyphRunAnalysis_Release(a);
    }
    check("outline mode cannot be rasterized",IDWriteFactory_CreateGlyphRunAnalysis(f,&run,1.0f,NULL,DWRITE_RENDERING_MODE_OUTLINE,DWRITE_MEASURING_MODE_NATURAL,0.0f,0.0f,&a)!=S_OK);
}

static void system_checks(IDWriteFactory *f) {
    IDWriteFontCollection *c=NULL;
    check("system collection",IDWriteFactory_GetSystemFontCollection(f,&c,FALSE)==S_OK && c);
    if(!c) return;
    UINT32 n=IDWriteFontCollection_GetFontFamilyCount(c);
    check("system families",n>=1);
    UINT32 index=7; BOOL exists=TRUE;
    check("an unknown family",IDWriteFontCollection_FindFamilyName(c,L"No Such Family",&index,&exists)==S_OK && !exists && index==UINT_MAX);
    IDWriteFontFamily *fam=NULL;
    check("first family",IDWriteFontCollection_GetFontFamily(c,0,&fam)==S_OK && fam);
    if(fam) {
        IDWriteLocalizedStrings *names=NULL; UINT32 len=0; WCHAR buf[128];
        check("family names",IDWriteFontFamily_GetFamilyNames(fam,&names)==S_OK && names && IDWriteLocalizedStrings_GetCount(names)>=1);
        if(names) {
            UINT32 at=9; BOOL has=FALSE;
            check("English family name",IDWriteLocalizedStrings_FindLocaleName(names,L"en-us",&at,&has)==S_OK && has && at<IDWriteLocalizedStrings_GetCount(names));
            check("family name text",has && IDWriteLocalizedStrings_GetStringLength(names,at,&len)==S_OK && len>0 && len<128 &&
                  IDWriteLocalizedStrings_GetString(names,at,buf,128)==S_OK && buf[len]==0);
            WCHAR part[128];
            check("short buffer",!has || (IDWriteLocalizedStrings_GetString(names,at,part,len)==E_NOT_SUFFICIENT_BUFFER && part[len-1]==0 && part[0]==buf[0]));
            if(has) {
                UINT32 found=99; BOOL ok=FALSE;
                HRESULT hr=IDWriteFontCollection_FindFamilyName(c,buf,&found,&ok);
                if(!(hr==S_OK && ok)){ put("       family: "); char t[128]; int k=0; while(buf[k]&&k<127){t[k]=(char)buf[k];k++;} t[k]=0; put(t); put("\n"); }
                check("find the family by name",hr==S_OK && ok && found<n);
            }
            IDWriteLocalizedStrings_Release(names);
        }
        IDWriteFont *font=NULL;
        check("matching font",IDWriteFontFamily_GetFirstMatchingFont(fam,DWRITE_FONT_WEIGHT_NORMAL,DWRITE_FONT_STRETCH_NORMAL,DWRITE_FONT_STYLE_NORMAL,&font)==S_OK && font);
        if(font) {
            BOOL has=FALSE;
            check("font has 'a'",IDWriteFont_HasCharacter(font,'a',&has)==S_OK && has);
            check("weight",IDWriteFont_GetWeight(font)>=100 && IDWriteFont_GetWeight(font)<=950);
            IDWriteFontFace *ff=NULL; IDWriteFont *back=NULL;
            check("font's face",IDWriteFont_CreateFontFace(font,&ff)==S_OK && ff);
            if(ff) {
                check("font from its face",IDWriteFontCollection_GetFontFromFontFace(c,ff,&back)==S_OK && back);
                if(back) IDWriteFont_Release(back);
                IDWriteFontFace_Release(ff);
            }
            IDWriteLocalizedStrings *full=NULL; BOOL ex=FALSE;
            check("full name",IDWriteFont_GetInformationalStrings(font,DWRITE_INFORMATIONAL_STRING_FULL_NAME,&full,&ex)==S_OK && (!ex || full));
            if(full) IDWriteLocalizedStrings_Release(full);
            IDWriteFont_Release(font);
        }
        IDWriteFontFamily_Release(fam);
    }
    IDWriteGdiInterop *gdi=NULL;
    check("GDI interop",IDWriteFactory_GetGdiInterop(f,&gdi)==S_OK && gdi);
    if(gdi) {
        LOGFONTW lf={0}; lf.lfWeight=FW_BOLD;
        const WCHAR *arial=L"Arial";
        for(int i=0;arial[i];i++) lf.lfFaceName[i]=arial[i];
        IDWriteFont *font=NULL;
        check("Arial from a LOGFONT",IDWriteGdiInterop_CreateFontFromLOGFONT(gdi,&lf,&font)==S_OK && font);
        if(font) {
            check("bold as asked",IDWriteFont_GetWeight(font)>=600);
            LOGFONTW out; BOOL sys=FALSE;
            check("back to a LOGFONT",IDWriteGdiInterop_ConvertFontToLOGFONT(gdi,font,&out,&sys)==S_OK && out.lfWeight>=600 && out.lfFaceName[0]);
            IDWriteFont_Release(font);
        }
        IDWriteGdiInterop_Release(gdi);
    }
    IDWriteFontCollection_Release(c);
}

void __stdcall start(void) {
    IDWriteFactory *f=NULL;
    check("factory",DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED,&iid_factory,(IUnknown **)&f)==S_OK && f);
    if(!f) finish("dwrite");
    IDWriteRenderingParams *p=NULL;
    check("custom rendering parameters",IDWriteFactory_CreateCustomRenderingParams(f,2.2f,0.75f,0.5f,DWRITE_PIXEL_GEOMETRY_BGR,DWRITE_RENDERING_MODE_ALIASED,&p)==S_OK && p);
    if(p) {
        check("gamma",IDWriteRenderingParams_GetGamma(p)==2.2f);
        check("enhanced contrast",IDWriteRenderingParams_GetEnhancedContrast(p)==0.75f);
        check("ClearType level",IDWriteRenderingParams_GetClearTypeLevel(p)==0.5f);
        check("pixel geometry",IDWriteRenderingParams_GetPixelGeometry(p)==DWRITE_PIXEL_GEOMETRY_BGR);
        check("rendering mode",IDWriteRenderingParams_GetRenderingMode(p)==DWRITE_RENDERING_MODE_ALIASED);
        IDWriteRenderingParams_Release(p);
    }
    check("bad gamma",IDWriteFactory_CreateCustomRenderingParams(f,0.0f,0.5f,0.5f,DWRITE_PIXEL_GEOMETRY_RGB,DWRITE_RENDERING_MODE_DEFAULT,&p)==E_INVALIDARG);
    WCHAR path[MAX_PATH];
    DWORD n=GetEnvironmentVariableW(L"OSYX_TEST_FONT",path,MAX_PATH);
    if(!n) { const WCHAR *def=L"C:\\Windows\\Fonts\\Lato-Regular.ttf"; for(n=0;def[n];n++) path[n]=def[n]; path[n]=0; }
    IDWriteFontFile *none=NULL;
    check("missing font file",IDWriteFactory_CreateFontFileReference(f,L"C:\\no\\such.ttf",NULL,&none)==DWRITE_E_FILENOTFOUND && !none);
    IDWriteFontFace *face=face_from_file(f,path);
    if(face) {
        face_checks(face);
        analysis_checks(f,face);
        IDWriteFontFace_Release(face);
    }
    system_checks(f);
    (void)wide_eq;
    IDWriteFactory_Release(f);
    finish("dwrite");
}
