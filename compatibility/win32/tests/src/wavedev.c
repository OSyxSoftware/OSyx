/* winmm waveform devices: enumeration, capabilities, recording and
 * playback with format conversion. With no sound card every device id is
 * MMSYSERR_BADDEVICEID; with one, buffers recorded at 44.1 kHz mono and
 * 22.05 kHz 8-bit stereo come back full and at the card's pace, playback
 * completes, and recording and playback run at the same time.
 * The same PE runs on Windows. */
#include "common.h"
#include <mmsystem.h>

static WAVEFORMATEX pcm(DWORD rate, WORD channels, WORD bits) {
    WAVEFORMATEX f;
    f.wFormatTag = WAVE_FORMAT_PCM;
    f.nChannels = channels;
    f.nSamplesPerSec = rate;
    f.wBitsPerSample = bits;
    f.nBlockAlign = (WORD)(channels * bits / 8);
    f.nAvgBytesPerSec = rate * f.nBlockAlign;
    f.cbSize = 0;
    return f;
}

static BYTE rec_a[44100 / 4 * 2], rec_b[44100 / 4 * 2];
static BYTE rec_c[22050 / 10 * 2];
static BYTE tone[22050 / 5];

/* Whether the first `n` output (or input) devices all have different names. */
static int distinct_names(UINT n, int output) {
    WCHAR names[8][MAXPNAMELEN];
    if (n > 8) n = 8;
    for (UINT i = 0; i < n; i++) {
        WAVEOUTCAPSW oc;
        WAVEINCAPSW ic;
        const WCHAR *name;
        if (output) {
            if (waveOutGetDevCapsW(i, &oc, sizeof oc) != MMSYSERR_NOERROR) return 0;
            name = oc.szPname;
        } else {
            if (waveInGetDevCapsW(i, &ic, sizeof ic) != MMSYSERR_NOERROR) return 0;
            name = ic.szPname;
        }
        for (int k = 0; k < MAXPNAMELEN; k++) names[i][k] = name[k];
        for (UINT j = 0; j < i; j++) {
            int same = 1;
            for (int k = 0; k < MAXPNAMELEN && (names[i][k] || names[j][k]); k++)
                if (names[i][k] != names[j][k]) { same = 0; break; }
            if (same) return 0;
        }
    }
    return 1;
}

/* Waits until every header in `h` is done, up to `ms` milliseconds. */
static int wait_done(HANDLE ev, WAVEHDR **h, int n, DWORD ms) {
    DWORD start = GetTickCount();
    for (;;) {
        int done = 1;
        for (int i = 0; i < n; i++)
            if (!(h[i]->dwFlags & WHDR_DONE)) done = 0;
        if (done) return 1;
        if (GetTickCount() - start > ms) return 0;
        WaitForSingleObject(ev, 50);
    }
}

/* Records two quarter seconds at 44.1 kHz mono 16-bit from `id`. */
static void record_44k_mono(UINT id, const char *what) {
    WAVEFORMATEX f = pcm(44100, 1, 16);
    HANDLE ev = CreateEventA(NULL, FALSE, FALSE, NULL);
    HWAVEIN in;
    MMRESULT r = waveInOpen(&in, id, &f, (DWORD_PTR)ev, 0, CALLBACK_EVENT);
    check(what, r == MMSYSERR_NOERROR);
    if (r != MMSYSERR_NOERROR) return;
    WAVEHDR a = {0}, b = {0};
    a.lpData = (LPSTR)rec_a; a.dwBufferLength = sizeof rec_a;
    b.lpData = (LPSTR)rec_b; b.dwBufferLength = sizeof rec_b;
    check("waveInPrepareHeader", waveInPrepareHeader(in, &a, sizeof a) == MMSYSERR_NOERROR
        && waveInPrepareHeader(in, &b, sizeof b) == MMSYSERR_NOERROR);
    check("waveInAddBuffer", waveInAddBuffer(in, &a, sizeof a) == MMSYSERR_NOERROR
        && waveInAddBuffer(in, &b, sizeof b) == MMSYSERR_NOERROR);
    DWORD t0 = GetTickCount();
    check("waveInStart", waveInStart(in) == MMSYSERR_NOERROR);
    WAVEHDR *hs[2] = {&a, &b};
    int done = wait_done(ev, hs, 2, 10000);
    DWORD elapsed = GetTickCount() - t0;
    check("both buffers come back recorded", done);
    check_eq("first buffer is full", a.dwBytesRecorded, sizeof rec_a);
    check_eq("second buffer is full", b.dwBytesRecorded, sizeof rec_b);
    /* Half a second of sound cannot arrive much faster than real time. */
    check("recording keeps the card's pace", elapsed >= 400);
    MMTIME pos;
    pos.wType = TIME_BYTES;
    check("waveInGetPosition counts the recorded bytes",
        waveInGetPosition(in, &pos, sizeof pos) == MMSYSERR_NOERROR && pos.wType == TIME_BYTES
        && pos.u.cb >= sizeof rec_a + sizeof rec_b);
    check("waveInStop", waveInStop(in) == MMSYSERR_NOERROR);
    check("waveInReset", waveInReset(in) == MMSYSERR_NOERROR);
    check("waveInUnprepareHeader", waveInUnprepareHeader(in, &a, sizeof a) == MMSYSERR_NOERROR
        && waveInUnprepareHeader(in, &b, sizeof b) == MMSYSERR_NOERROR);
    check("waveInClose", waveInClose(in) == MMSYSERR_NOERROR);
    CloseHandle(ev);
}

void __stdcall start(void) {
    UINT outs = waveOutGetNumDevs(), ins = waveInGetNumDevs();
    put("wavedev: ");
    put_uint(outs);
    put(" output and ");
    put_uint(ins);
    put(" input devices\r\n");
    /* Each card is one device: no two devices share a name. */
    check("output device names are distinct", distinct_names(outs, 1));
    check("input device names are distinct", distinct_names(ins, 0));

    WAVEFORMATEX f = pcm(44100, 2, 16);
    HWAVEOUT out;
    HWAVEIN in;
    check_eq("waveOutOpen past the last device", waveOutOpen(&out, outs, &f, 0, 0, CALLBACK_NULL), MMSYSERR_BADDEVICEID);
    check_eq("waveInOpen past the last device", waveInOpen(&in, ins, &f, 0, 0, CALLBACK_NULL), MMSYSERR_BADDEVICEID);
    WAVEOUTCAPSW oc;
    WAVEINCAPSW ic;
    check_eq("waveOutGetDevCaps past the last device", waveOutGetDevCapsW(outs, &oc, sizeof oc), MMSYSERR_BADDEVICEID);
    check_eq("waveInGetDevCaps past the last device", waveInGetDevCapsW(ins, &ic, sizeof ic), MMSYSERR_BADDEVICEID);

    if (outs > 0) {
        check("waveOutGetDevCaps names the first output", waveOutGetDevCapsW(0, &oc, sizeof oc) == MMSYSERR_NOERROR
            && oc.szPname[0] != 0 && oc.wChannels >= 1);
        check("the wave mapper has output capabilities", waveOutGetDevCapsW(WAVE_MAPPER, &oc, sizeof oc) == MMSYSERR_NOERROR);
    }
    if (ins > 0) {
        check("waveInGetDevCaps names the first input", waveInGetDevCapsW(0, &ic, sizeof ic) == MMSYSERR_NOERROR
            && ic.szPname[0] != 0 && ic.wChannels >= 1);
        check("the wave mapper has input capabilities", waveInGetDevCapsW(WAVE_MAPPER, &ic, sizeof ic) == MMSYSERR_NOERROR);
        WAVEFORMATEX q = pcm(44100, 1, 16);
        check("44.1 kHz mono recording is supported", waveInOpen(NULL, 0, &q, 0, 0, WAVE_FORMAT_QUERY) == MMSYSERR_NOERROR);
        record_44k_mono(WAVE_MAPPER, "waveInOpen on the wave mapper at 44.1 kHz mono");
    }

    if (outs > 0 && ins > 0) {
        /* Duplex: 22.05 kHz 8-bit stereo recording from device 0 while
         * 22.05 kHz 8-bit mono plays on device 0. */
        HANDLE ev_in = CreateEventA(NULL, FALSE, FALSE, NULL), ev_out = CreateEventA(NULL, FALSE, FALSE, NULL);
        WAVEFORMATEX rf = pcm(22050, 2, 8), pf = pcm(22050, 1, 8);
        int ok_in = waveInOpen(&in, 0, &rf, (DWORD_PTR)ev_in, 0, CALLBACK_EVENT) == MMSYSERR_NOERROR;
        int ok_out = waveOutOpen(&out, 0, &pf, (DWORD_PTR)ev_out, 0, CALLBACK_EVENT) == MMSYSERR_NOERROR;
        check("recording and playback open together", ok_in && ok_out);
        if (ok_in && ok_out) {
            for (DWORD i = 0; i < sizeof tone; i++) tone[i] = (BYTE)((i / 25) % 2 ? 160 : 96);
            WAVEHDR r = {0}, p = {0};
            r.lpData = (LPSTR)rec_c; r.dwBufferLength = sizeof rec_c;
            p.lpData = (LPSTR)tone; p.dwBufferLength = sizeof tone;
            waveInPrepareHeader(in, &r, sizeof r);
            waveInAddBuffer(in, &r, sizeof r);
            waveInStart(in);
            waveOutPrepareHeader(out, &p, sizeof p);
            check("waveOutWrite", waveOutWrite(out, &p, sizeof p) == MMSYSERR_NOERROR);
            WAVEHDR *ph[1] = {&p}, *rh[1] = {&r};
            check("the played buffer comes back", wait_done(ev_out, ph, 1, 10000));
            check("the 8-bit stereo buffer comes back full", wait_done(ev_in, rh, 1, 10000) && r.dwBytesRecorded == sizeof rec_c);
            waveInReset(in);
            waveOutReset(out);
            waveInUnprepareHeader(in, &r, sizeof r);
            waveOutUnprepareHeader(out, &p, sizeof p);
        }
        if (ok_in) check("waveInClose after duplex", waveInClose(in) == MMSYSERR_NOERROR);
        if (ok_out) check("waveOutClose after duplex", waveOutClose(out) == MMSYSERR_NOERROR);
        CloseHandle(ev_in);
        CloseHandle(ev_out);
    }

    finish("wavedev");
}
