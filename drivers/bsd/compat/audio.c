/*
 * Audio for OSyx on top of OpenBSD audio drivers (azalia(4)): the part of
 * OpenBSD's audio(4) that drives a hardware ring buffer through
 * struct audio_hw_if. Playback only, fixed format: 48 kHz, signed 16-bit
 * little-endian, stereo. The kernel (kernel/src/audio.rs) supplies the
 * samples block by block through osyx_bsd_audio_fill.
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

struct audio_dev {
	const struct audio_hw_if *hw;
	void *hdl;
	struct device *dev;
	uint32_t pciid;
	int running;
	char *ring;
	int blksz, nblks, next;
	struct audio_params play;
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

/* A block has been played: refill it, it is now the furthest ahead. */
static void audio_intr(void *arg)
{
	struct audio_dev *a = arg;
	if (!a->running)
		return;
	osyx_bsd_audio_fill(a - devs, a->ring + a->next * a->blksz, a->blksz);
	a->next = (a->next + 1) % a->nblks;
}

/* Opens the device and starts the ring. Returns 0 or an errno. */
int osyx_bsd_audio_start(int unit, int *rate)
{
	if (unit < 0 || unit >= ndevs)
		return ENXIO;
	struct audio_dev *a = &devs[unit];
	if (a->running)
		return 0;
	const struct audio_hw_if *hw = a->hw;
	int e = hw->open(a->hdl, FWRITE);
	if (e)
		return e;
	struct audio_params p = { .sample_rate = RATE, .encoding = AUDIO_ENCODING_SLINEAR_LE,
	    .precision = 16, .bps = 2, .msb = 1, .channels = CHANNELS };
	struct audio_params r = p;
	if ((e = hw->set_params(a->hdl, AUMODE_PLAY, AUMODE_PLAY, &p, &r)) != 0)
		goto fail;
	if (p.encoding != AUDIO_ENCODING_SLINEAR_LE || p.precision != 16 || p.bps != 2 || p.channels != CHANNELS) {
		printf("%s: unsupported format (%u-bit, %u channels)\n", a->dev->dv_xname, p.precision, p.channels);
		e = EINVAL;
		goto fail;
	}
	int blksz = WANT_BLOCK, nblks = WANT_BLOCKS;
	if (hw->set_blksz)
		blksz = hw->set_blksz(a->hdl, AUMODE_PLAY, &p, &r, blksz);
	if (hw->round_blocksize)
		blksz = hw->round_blocksize(a->hdl, blksz);
	if (hw->set_nblks)
		nblks = hw->set_nblks(a->hdl, AUMODE_PLAY, &p, blksz, nblks);
	size_t size = (size_t)blksz * nblks;
	if (hw->round_buffersize)
		size = hw->round_buffersize(a->hdl, AUMODE_PLAY, size);
	nblks = size / blksz;
	a->ring = hw->allocm(a->hdl, AUMODE_PLAY, (size_t)blksz * nblks, M_DEVBUF, M_WAITOK | M_ZERO);
	if (a->ring == NULL) {
		e = ENOMEM;
		goto fail;
	}
	a->blksz = blksz;
	a->nblks = nblks;
	a->next = 0;
	a->play = p;
	for (int i = 0; i < nblks; i++)
		osyx_bsd_audio_fill(unit, a->ring + i * blksz, blksz);
	a->running = 1;
	mtx_enter(&audio_lock);
	e = hw->trigger_output(a->hdl, a->ring, a->ring + blksz * nblks, blksz, audio_intr, a, &a->play);
	mtx_leave(&audio_lock);
	if (e) {
		a->running = 0;
		hw->freem(a->hdl, a->ring, M_DEVBUF);
		a->ring = NULL;
		goto fail;
	}
	printf("%s: playing %lu Hz, %d blocks of %d bytes\n", a->dev->dv_xname, p.sample_rate, nblks, blksz);
	*rate = p.sample_rate;
	return 0;
fail:
	hw->close(a->hdl);
	return e;
}

void osyx_bsd_audio_stop(int unit)
{
	if (unit < 0 || unit >= ndevs || !devs[unit].running)
		return;
	struct audio_dev *a = &devs[unit];
	mtx_enter(&audio_lock);
	a->running = 0;
	a->hw->halt_output(a->hdl);
	mtx_leave(&audio_lock);
	a->hw->freem(a->hdl, a->ring, M_DEVBUF);
	a->ring = NULL;
	a->hw->close(a->hdl);
}
