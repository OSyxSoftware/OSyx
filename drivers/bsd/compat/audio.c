/*
 * Audio for OSyx on top of OpenBSD audio drivers (azalia(4)): the part of
 * OpenBSD's audio(4) that drives the hardware ring buffers through
 * struct audio_hw_if. Playback and recording, each in one fixed format:
 * 48 kHz, signed 16-bit little-endian, stereo. The kernel
 * (kernel/src/audio.rs) supplies the samples to play block by block
 * through osyx_bsd_audio_fill and receives the recorded ones through
 * osyx_bsd_audio_take.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */
#include <osyx_bsd.h>
#include <sys/audioio.h>
#include <dev/audio_if.h>

struct mutex audio_lock = MUTEX_INITIALIZER(IPL_AUDIO);

#define MAX_AUDIO 4
#define RATE 48000
#define CHANNELS 2
#define FRAME_BYTES (CHANNELS * 2)
/* About 10 ms per block, 8 blocks (RING_BLOCKS in kernel/src/audio.rs):
 * 80 ms of buffering in the hardware. */
#define WANT_BLOCK (RATE / 100 * FRAME_BYTES)
#define WANT_BLOCKS 8

/* One direction of a device: its hardware ring. */
struct audio_ring {
	int running;
	char *ring;
	int blksz, nblks, next;
	struct audio_params params;
};

struct audio_dev {
	const struct audio_hw_if *hw;
	void *hdl;
	struct device *dev;
	uint32_t pciid;
	struct audio_ring play, rec;
};

static struct audio_dev devs[MAX_AUDIO];
static int ndevs;

/* Set by osyx_bsd_pci_attach while a driver attaches. */
extern uint32_t osyx_bsd_attaching_id;

/*
 * Response acknowledgement while azalia polls for codec responses with
 * audio_lock held (interrupts blocked): azalia programs RINTCNT = 1, and
 * some controllers (QEMU's intel-hda) stop writing responses until the
 * RIRB interrupt flag is cleared, which only the blocked interrupt handler
 * would do. Clearing the flag loses nothing: responses stay in the RIRB.
 */
#define MAX_HDA 4
#define HDA_RIRBSTS 0x5d
#define HDA_RIRBSTS_RINTFL 0x01
static uint64_t hda_regs[MAX_HDA];

void osyx_bsd_hda_regs(uint64_t va)
{
	for (int i = 0; i < MAX_HDA; i++)
		if (hda_regs[i] == 0 || hda_regs[i] == va) {
			hda_regs[i] = va;
			return;
		}
}

void osyx_bsd_hda_kick(void)
{
	if (audio_lock.mtx_depth <= 0)
		return;
	for (int i = 0; i < MAX_HDA && hda_regs[i]; i++) {
		volatile uint8_t *sts = (volatile uint8_t *)(hda_regs[i] + HDA_RIRBSTS);
		if (*sts & HDA_RIRBSTS_RINTFL)
			*sts = HDA_RIRBSTS_RINTFL;
	}
}

/* Kernel side: copies the next `len` bytes of PCM (or silence) to `dst`. */
void osyx_bsd_audio_fill(int unit, void *dst, int len);
/* Kernel side: takes `len` bytes of recorded PCM from `src`. */
void osyx_bsd_audio_take(int unit, const void *src, int len);

struct device *audio_attach_mi(const struct audio_hw_if *hw, void *hdl, void *cookie, struct device *dev)
{
	(void)cookie;
	if (ndevs == MAX_AUDIO) {
		printf("%s: too many audio devices, ignored\n", dev->dv_xname);
		return NULL;
	}
	struct audio_dev *a = &devs[ndevs];
	a->hw = hw;
	a->hdl = hdl;
	a->dev = dev;
	a->pciid = osyx_bsd_attaching_id;
	printf("audio%d at %s\n", ndevs, dev->dv_xname);
	ndevs++;
	return dev;
}

int audioprint(void *aux, const char *pnp) { (void)aux; (void)pnp; return UNCONF; }

int audio_blksz_bytes(int mode, struct audio_params *p, struct audio_params *r, int frames)
{
	struct audio_params *pr = (mode == AUMODE_PLAY) ? p : r;
	return frames * pr->bps * pr->channels;
}

int osyx_bsd_audio_count(void) { return ndevs; }

/* Name ("azalia0") and PCI id of an audio device. */
uint32_t osyx_bsd_audio_info(int unit, char *name, size_t cap)
{
	if (unit < 0 || unit >= ndevs)
		return 0;
	strlcpy(name, devs[unit].dev->dv_xname, cap);
	return devs[unit].pciid;
}

/*
 * What a device can do: bit 0 playback, bit 1 recording. The driver's
 * open() refuses a direction its codec has no converters for (azalia: no
 * DAC or no ADC group), and is otherwise a reference count.
 */
int osyx_bsd_audio_caps(int unit)
{
	if (unit < 0 || unit >= ndevs)
		return 0;
	struct audio_dev *a = &devs[unit];
	int caps = 0;
	if (a->hw->open(a->hdl, FWRITE) == 0) {
		a->hw->close(a->hdl);
		caps |= 1;
	}
	if (a->hw->open(a->hdl, FREAD) == 0) {
		a->hw->close(a->hdl);
		caps |= 2;
	}
	return caps;
}

/* A block has been played: refill it, it is now the furthest ahead. */
static void play_intr(void *arg)
{
	struct audio_dev *a = arg;
	struct audio_ring *r = &a->play;
	if (!r->running)
		return;
	osyx_bsd_audio_fill(a - devs, r->ring + r->next * r->blksz, r->blksz);
	r->next = (r->next + 1) % r->nblks;
}

/* A block has been recorded: hand it over, the hardware reuses it later. */
static void rec_intr(void *arg)
{
	struct audio_dev *a = arg;
	struct audio_ring *r = &a->rec;
	if (!r->running)
		return;
	osyx_bsd_audio_take(a - devs, r->ring + r->next * r->blksz, r->blksz);
	r->next = (r->next + 1) % r->nblks;
}

/*
 * Opens one direction (AUMODE_PLAY or AUMODE_RECORD) and starts its ring.
 * Returns 0 or an errno.
 */
static int start(int unit, int mode, int *rate)
{
	if (unit < 0 || unit >= ndevs)
		return ENXIO;
	struct audio_dev *a = &devs[unit];
	struct audio_ring *r = mode == AUMODE_PLAY ? &a->play : &a->rec;
	if (r->running)
		return 0;
	const struct audio_hw_if *hw = a->hw;
	int e = hw->open(a->hdl, mode == AUMODE_PLAY ? FWRITE : FREAD);
	if (e)
		return e;
	struct audio_params want = { .sample_rate = RATE, .encoding = AUDIO_ENCODING_SLINEAR_LE,
	    .precision = 16, .bps = 2, .msb = 1, .channels = CHANNELS };
	struct audio_params p = want, rp = want;
	if ((e = hw->set_params(a->hdl, mode, mode, &p, &rp)) != 0)
		goto fail;
	struct audio_params *got = mode == AUMODE_PLAY ? &p : &rp;
	if (got->encoding != AUDIO_ENCODING_SLINEAR_LE || got->precision != 16 || got->bps != 2 || got->channels != CHANNELS) {
		printf("%s: unsupported %s format (%u-bit, %u channels)\n", a->dev->dv_xname,
		    mode == AUMODE_PLAY ? "playback" : "recording", got->precision, got->channels);
		e = EINVAL;
		goto fail;
	}
	int blksz = WANT_BLOCK, nblks = WANT_BLOCKS;
	if (hw->set_blksz)
		blksz = hw->set_blksz(a->hdl, mode, &p, &rp, blksz);
	if (hw->round_blocksize)
		blksz = hw->round_blocksize(a->hdl, blksz);
	if (hw->set_nblks)
		nblks = hw->set_nblks(a->hdl, mode, got, blksz, nblks);
	size_t size = (size_t)blksz * nblks;
	if (hw->round_buffersize)
		size = hw->round_buffersize(a->hdl, mode, size);
	nblks = size / blksz;
	r->ring = hw->allocm(a->hdl, mode, (size_t)blksz * nblks, M_DEVBUF, M_WAITOK | M_ZERO);
	if (r->ring == NULL) {
		e = ENOMEM;
		goto fail;
	}
	r->blksz = blksz;
	r->nblks = nblks;
	r->next = 0;
	r->params = *got;
	if (mode == AUMODE_PLAY)
		for (int i = 0; i < nblks; i++)
			osyx_bsd_audio_fill(unit, r->ring + i * blksz, blksz);
	r->running = 1;
	mtx_enter(&audio_lock);
	if (mode == AUMODE_PLAY)
		e = hw->trigger_output(a->hdl, r->ring, r->ring + blksz * nblks, blksz, play_intr, a, &r->params);
	else if (hw->trigger_input)
		e = hw->trigger_input(a->hdl, r->ring, r->ring + blksz * nblks, blksz, rec_intr, a, &r->params);
	else
		e = ENODEV;
	mtx_leave(&audio_lock);
	if (e) {
		r->running = 0;
		hw->freem(a->hdl, r->ring, M_DEVBUF);
		r->ring = NULL;
		goto fail;
	}
	printf("%s: %s %lu Hz, %d blocks of %d bytes\n", a->dev->dv_xname,
	    mode == AUMODE_PLAY ? "playing" : "recording", r->params.sample_rate, nblks, blksz);
	*rate = r->params.sample_rate;
	return 0;
fail:
	hw->close(a->hdl);
	return e;
}

static void stop(int unit, int mode)
{
	if (unit < 0 || unit >= ndevs)
		return;
	struct audio_dev *a = &devs[unit];
	struct audio_ring *r = mode == AUMODE_PLAY ? &a->play : &a->rec;
	if (!r->running)
		return;
	mtx_enter(&audio_lock);
	r->running = 0;
	if (mode == AUMODE_PLAY)
		a->hw->halt_output(a->hdl);
	else
		a->hw->halt_input(a->hdl);
	mtx_leave(&audio_lock);
	a->hw->freem(a->hdl, r->ring, M_DEVBUF);
	r->ring = NULL;
	a->hw->close(a->hdl);
}

int osyx_bsd_audio_start(int unit, int *rate) { return start(unit, AUMODE_PLAY, rate); }
void osyx_bsd_audio_stop(int unit) { stop(unit, AUMODE_PLAY); }
int osyx_bsd_audio_start_rec(int unit, int *rate) { return start(unit, AUMODE_RECORD, rate); }
void osyx_bsd_audio_stop_rec(int unit) { stop(unit, AUMODE_RECORD); }
