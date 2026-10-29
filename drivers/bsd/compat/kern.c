/*
 * OSyx compatibility layer, part 2: the kernel interfaces net80211 and
 * iwm(4) need beyond the Ethernet drivers (sleeping, firmware, deferred
 * attach, time, randomness, more mbuf and queue operations).
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */
#include <osyx_bsd.h>

/* compat.c */
struct mbuf *osyx_bsd_m_alloc(size_t bufsize, int flags);
void osyx_bsd_run_due(void);
void osyx_bsd_run_tasks(void);
void osyx_bsd_start(struct ifnet *);
/* kernel/src/bsd.rs */
void osyx_bsd_yield(void);

int cold;
const uint8_t etheranyaddr[ETHER_ADDR_LEN];

void explicit_bzero(void *p, size_t n)
{
	volatile uint8_t *v = p;
	while (n--)
		*v++ = 0;
}

int timingsafe_bcmp(const void *a, const void *b, size_t n)
{
	const uint8_t *x = a, *y = b;
	uint8_t d = 0;
	while (n--)
		d |= *x++ ^ *y++;
	return d != 0;
}

void arc4random_buf(void *buf, size_t n) { osyx_bsd_random(buf, n); }
uint32_t arc4random(void) { uint32_t v; osyx_bsd_random(&v, sizeof(v)); return v; }

uint32_t arc4random_uniform(uint32_t bound)
{
	if (bound < 2)
		return 0;
	uint32_t min = -bound % bound, r;
	do { r = arc4random(); } while (r < min);
	return r % bound;
}

uint32_t ether_crc32_le_update(uint32_t crc, const uint8_t *buf, size_t len)
{
	for (size_t i = 0; i < len; i++) {
		crc ^= buf[i];
		for (int b = 0; b < 8; b++)
			crc = (crc >> 1) ^ (crc & 1 ? 0xedb88320 : 0);
	}
	return crc;
}

uint32_t ether_crc32_be_update(uint32_t crc, const uint8_t *buf, size_t len)
{
	for (size_t i = 0; i < len; i++) {
		uint8_t c = buf[i];
		for (int b = 0; b < 8; b++, c >>= 1) {
			uint32_t carry = ((crc & 0x80000000) ? 1 : 0) ^ (c & 1);
			crc <<= 1;
			if (carry)
				crc = (crc ^ 0x04c11db6) | carry;
		}
	}
	return crc;
}

/* ---- time: one tick is one millisecond ---- */

uint64_t nsecuptime(void) { return osyx_bsd_ticks() * 1000000ULL; }

void microuptime(struct timeval *tv)
{
	uint64_t ms = osyx_bsd_ticks();
	tv->tv_sec = (time_t)(ms / 1000);
	tv->tv_usec = (long)(ms % 1000) * 1000;
}
void getmicrouptime(struct timeval *tv) { microuptime(tv); }
void microtime(struct timeval *tv) { microuptime(tv); }
void getmicrotime(struct timeval *tv) { microuptime(tv); }

void nanouptime(struct timespec *ts)
{
	uint64_t ms = osyx_bsd_ticks();
	ts->tv_sec = (time_t)(ms / 1000);
	ts->tv_nsec = (long)(ms % 1000) * 1000000;
}
void getnanouptime(struct timespec *ts) { nanouptime(ts); }
time_t getuptime(void) { return (time_t)(osyx_bsd_ticks() / 1000); }
time_t gettime(void) { return getuptime(); }

int ratecheck(struct timeval *last, const struct timeval *interval)
{
	struct timeval now, delta;
	getmicrouptime(&now);
	timersub(&now, last, &delta);
	if (timercmp(&delta, interval, >=) || (last->tv_sec == 0 && last->tv_usec == 0)) {
		*last = now;
		return 1;
	}
	return 0;
}

void log(int level, const char *fmt, ...)
{
	(void)level;
	va_list ap;
	va_start(ap, fmt);
	vprintf(fmt, ap);
	va_end(ap);
}

void addlog(const char *fmt, ...)
{
	va_list ap;
	va_start(ap, fmt);
	vprintf(fmt, ap);
	va_end(ap);
}

/* ---- sleeping ---- */

#define MAX_WAKEUPS 32
static const volatile void *woken[MAX_WAKEUPS];

void wakeup(const volatile void *ident)
{
	for (int i = 0; i < MAX_WAKEUPS; i++)
		if (woken[i] == ident)
			return;
	for (int i = 0; i < MAX_WAKEUPS; i++) {
		if (woken[i] == NULL) {
			woken[i] = ident;
			return;
		}
	}
}

void wakeup_one(const volatile void *ident) { wakeup(ident); }

static int take_wakeup(const volatile void *ident)
{
	for (int i = 0; i < MAX_WAKEUPS; i++) {
		if (woken[i] == ident) {
			woken[i] = NULL;
			return 1;
		}
	}
	return 0;
}

/*
 * The big lock stays held while sleeping, so the sleeper runs what would
 * wake it: the polled interrupt handlers, expired timeouts and queued tasks
 * (iwm_init waits for its newstate task). Unbounded sleeps give up after
 * 10 s rather than hang the system. The first millisecond spins (command
 * responses arrive within microseconds); after that the sleeper gives the
 * CPU away a tick at a time, so the rest of the system keeps running.
 */
int tsleep_nsec(const volatile void *ident, int pri, const char *msg, uint64_t nsecs)
{
	(void)pri; (void)msg;
	uint64_t ms = nsecs == INFSLP ? 10000 : (nsecs + 999999) / 1000000;
	uint64_t start = osyx_bsd_ticks();
	uint64_t deadline = start + (ms ? ms : 1);
	take_wakeup(ident);
	for (;;) {
		osyx_bsd_run_due();
		osyx_bsd_run_tasks();
		if (take_wakeup(ident))
			return 0;
		uint64_t now = osyx_bsd_ticks();
		if (now >= deadline)
			return EWOULDBLOCK;
		if (now == start)
			delay(20);
		else
			osyx_bsd_yield();
	}
}

/* ---- task queues and threads ---- */

struct taskq *taskq_create(const char *name, unsigned int n, int ipl, unsigned int flags)
{
	(void)name; (void)n; (void)ipl; (void)flags;
	return systq;
}
void taskq_destroy(struct taskq *tq) { (void)tq; }

int kthread_create(void (*f)(void *), void *arg, struct proc **p, const char *name)
{
	(void)f; (void)arg; (void)p; (void)name;
	return ENOTSUP;
}

/* ---- firmware and deferred attach ---- */

int64_t osyx_bsd_file_size(const char *path);
int64_t osyx_bsd_file_read(const char *path, uint8_t *buf, size_t len);

int loadfirmware(const char *name, uint8_t **bufp, size_t *lenp)
{
	char path[128];
	snprintf(path, sizeof(path), "/etc/firmware/%s", name);
	int64_t size = osyx_bsd_file_size(path);
	if (size <= 0) {
		printf("loadfirmware: %s not found\n", path);
		return ENOENT;
	}
	uint8_t *buf = malloc((size_t)size, M_DEVBUF, M_NOWAIT);
	if (buf == NULL)
		return ENOMEM;
	if (osyx_bsd_file_read(path, buf, (size_t)size) != size) {
		free(buf, M_DEVBUF, (size_t)size);
		return EIO;
	}
	*bufp = buf;
	*lenp = (size_t)size;
	return 0;
}

#define MAX_MOUNTROOT 8
static struct { struct device *dev; void (*f)(struct device *); } mountroot_hooks[MAX_MOUNTROOT];
static int mounted;

void config_mountroot(struct device *dev, void (*f)(struct device *))
{
	if (mounted) {
		f(dev);
		return;
	}
	for (int i = 0; i < MAX_MOUNTROOT; i++) {
		if (mountroot_hooks[i].f == NULL) {
			mountroot_hooks[i].dev = dev;
			mountroot_hooks[i].f = f;
			return;
		}
	}
}

/* Called once the root file system (with /etc/firmware) is mounted. */
void osyx_bsd_mountroot(void)
{
	mounted = 1;
	for (int i = 0; i < MAX_MOUNTROOT; i++) {
		if (mountroot_hooks[i].f) {
			void (*f)(struct device *) = mountroot_hooks[i].f;
			mountroot_hooks[i].f = NULL;
			f(mountroot_hooks[i].dev);
		}
	}
}

/* ---- more mbuf and queue operations ---- */

int m_leadingspace(struct mbuf *m) { return (int)M_LEADINGSPACE(m); }
int m_trailingspace(struct mbuf *m) { return (int)M_TRAILINGSPACE(m); }
void m_purge(struct mbuf *m) { m_freem(m); }

void m_align(struct mbuf *m, int len)
{
	size_t room = m->m_ext.ext_size;
	if ((size_t)len < room)
		m->m_data = m->m_ext.ext_buf + ((room - (size_t)len) & ~(sizeof(long) - 1));
}

int m_dup_pkthdr(struct mbuf *to, struct mbuf *from, int how)
{
	(void)how;
	to->m_pkthdr = from->m_pkthdr;
	to->m_flags = (to->m_flags & M_EXT) | (from->m_flags & ~M_EXT) | M_PKTHDR;
	return 0;
}

/* Copies [off, off+len) of a chain into one new mbuf. */
struct mbuf *m_copym(struct mbuf *m, int off, int len, int how)
{
	(void)how;
	int total = 0;
	for (struct mbuf *x = m; x; x = x->m_next)
		total += x->m_len;
	if (len == M_COPYALL || off + len > total)
		len = total - off;
	if (len < 0)
		return NULL;
	struct mbuf *n = osyx_bsd_m_alloc((size_t)(len ? len : 1), m->m_flags & M_PKTHDR);
	if (n == NULL)
		return NULL;
	m_copydata(m, off, len, n->m_data);
	n->m_len = len;
	if (m->m_flags & M_PKTHDR) {
		n->m_pkthdr = m->m_pkthdr;
		n->m_pkthdr.len = len;
	}
	return n;
}

struct mbuf *m_dup_pkt(struct mbuf *m, unsigned int adj, int how)
{
	(void)how;
	struct mbuf *n = osyx_bsd_m_alloc((size_t)m->m_pkthdr.len + adj, M_PKTHDR);
	if (n == NULL)
		return NULL;
	n->m_data += adj;
	m_copydata(m, 0, m->m_pkthdr.len, n->m_data);
	n->m_len = m->m_pkthdr.len;
	n->m_pkthdr = m->m_pkthdr;
	return n;
}

/* Splits a chain after `len` bytes; the tail becomes one new mbuf. */
struct mbuf *m_split(struct mbuf *m, int len, int how)
{
	(void)how;
	int total = 0;
	for (struct mbuf *x = m; x; x = x->m_next)
		total += x->m_len;
	if (len > total)
		return NULL;
	struct mbuf *tail = osyx_bsd_m_alloc((size_t)(total - len ? total - len : 1), m->m_flags & M_PKTHDR);
	if (tail == NULL)
		return NULL;
	m_copydata(m, len, total - len, tail->m_data);
	tail->m_len = total - len;
	if (m->m_flags & M_PKTHDR) {
		tail->m_pkthdr = m->m_pkthdr;
		tail->m_pkthdr.len = total - len;
		m->m_pkthdr.len = len;
	}
	int keep = len;
	struct mbuf *x = m, *prev = NULL;
	for (; x && keep >= (int)x->m_len; prev = x, x = x->m_next)
		keep -= x->m_len;
	if (x != NULL) {
		x->m_len = keep;
		m_freem(x->m_next);
		x->m_next = NULL;
		if (keep == 0 && prev != NULL) {
			prev->m_next = NULL;
			m_free(x);
		}
	}
	return tail;
}

void m_cat(struct mbuf *m, struct mbuf *n)
{
	while (m->m_next)
		m = m->m_next;
	m->m_next = n;
}

void mq_init(struct mbuf_queue *mq, unsigned int maxlen, int ipl)
{
	(void)ipl;
	ml_init(&mq->mq_list);
	mq->mq_maxlen = maxlen;
	mq->mq_drops = 0;
}

int mq_enqueue(struct mbuf_queue *mq, struct mbuf *m)
{
	if (mq->mq_list.ml_len >= mq->mq_maxlen) {
		mq->mq_drops++;
		m_freem(m);
		return 1;
	}
	ml_enqueue(&mq->mq_list, m);
	return 0;
}

struct mbuf *mq_dequeue(struct mbuf_queue *mq) { return ml_dequeue(&mq->mq_list); }
unsigned int mq_purge(struct mbuf_queue *mq) { return ml_purge(&mq->mq_list); }

void mq_delist(struct mbuf_queue *mq, struct mbuf_list *ml)
{
	*ml = mq->mq_list;
	ml_init(&mq->mq_list);
}

/* ---- more interface operations ---- */

void if_start(struct ifnet *ifp) { osyx_bsd_start(ifp); }
void if_addgroup(struct ifnet *ifp, const char *g) { (void)ifp; (void)g; }
void if_deactivate(struct ifnet *ifp) { (void)ifp; }

int if_setlladdr(struct ifnet *ifp, const uint8_t *addr)
{
	memcpy(((struct arpcom *)ifp)->ac_enaddr, addr, ETHER_ADDR_LEN);
	return 0;
}

int if_enqueue(struct ifnet *ifp, struct mbuf *m)
{
	int e = ifq_enqueue(&ifp->if_snd, m);
	if (e == 0)
		osyx_bsd_start(ifp);
	return e;
}

int ether_output(struct ifnet *ifp, struct mbuf *m, struct sockaddr *dst, struct rtentry *rt)
{
	(void)dst; (void)rt;
	return if_enqueue(ifp, m);
}

int ether_addmulti(struct ifreq *ifr, struct arpcom *ac) { (void)ifr; (void)ac; return 0; }
int ether_delmulti(struct ifreq *ifr, struct arpcom *ac) { (void)ifr; (void)ac; return 0; }
