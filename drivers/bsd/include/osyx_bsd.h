/*
 * OSyx compatibility layer for OpenBSD network drivers.
 *
 * The OpenBSD kernel interfaces the drivers in ../openbsd use (types,
 * mbufs, bus_space/bus_dma, PCI, autoconf, ifnet/ifq, timeouts, tasks),
 * implemented on top of OSyx kernel services (osyx_bsd_* functions in
 * kernel/src/bsd.rs). Every header such a driver includes that is not
 * taken unchanged from OpenBSD (sys/queue.h, pcireg.h, pcidevs.h,
 * if_media.h, mii headers) maps to this file.
 *
 * Execution model: all driver code runs under one OSyx mutex (the "big
 * BSD lock"), from the attach path, the network thread and the BSD
 * service thread, so spl*, mutexes and the kernel lock are no-ops.
 * Interrupt handlers are polled by the service thread every tick.
 * DMA memory always lies below 4 GiB (some parts only address 32 bits).
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */
#ifndef OSYX_BSD_H
#define OSYX_BSD_H

#include <stddef.h>
#include <stdint.h>
#include <stdarg.h>

/* ---- types ---------------------------------------------------------------- */

typedef uint8_t u_int8_t;
typedef uint16_t u_int16_t;
typedef uint32_t u_int32_t;
typedef uint64_t u_int64_t;
typedef unsigned char u_char;
typedef unsigned short u_short;
typedef unsigned int u_int;
typedef unsigned long u_long;
typedef unsigned int uint;
typedef long ssize_t;
typedef char *caddr_t;
typedef uint64_t paddr_t;
typedef uint64_t vaddr_t;
typedef uint64_t bus_addr_t;
typedef uint64_t bus_size_t;
typedef int64_t off_t;
typedef uint64_t psize_t;
typedef uint64_t vsize_t;
typedef int64_t time_t;
typedef uint32_t in_addr_t;
typedef uint16_t sa_family_t;
typedef long suseconds_t;
struct rtentry;
struct proc;

#ifndef NULL
#define NULL ((void *)0)
#endif

#define __packed __attribute__((__packed__))
#define __aligned(x) __attribute__((__aligned__(x)))
#define __unused __attribute__((__unused__))
#define __predict_true(x) __builtin_expect(!!(x), 1)
#define __predict_false(x) __builtin_expect(!!(x), 0)
#define __BEGIN_DECLS
#define __END_DECLS
#define __KERNEL_RCSID(a, b)
#define __dead __attribute__((__noreturn__))
#define __bounded(...)
#define __attribute_const__ __attribute__((const))

#define nitems(a) (sizeof(a) / sizeof((a)[0]))
#define SET(t, f) ((t) |= (f))
#define CLR(t, f) ((t) &= ~(f))
#define ISSET(t, f) ((t) & (f))
#define MIN(a, b) (((a) < (b)) ? (a) : (b))
#define MAX(a, b) (((a) > (b)) ? (a) : (b))
#define min(a, b) MIN(a, b)
#define max(a, b) MAX(a, b)
#define ulmin(a, b) MIN(a, b)
#define howmany(x, y) (((x) + ((y) - 1)) / (y))
#define roundup(x, y) ((((x) + ((y) - 1)) / (y)) * (y))
#define powerof2(x) ((((x) - 1) & (x)) == 0)

#define PAGE_SIZE 4096
#define NBBY 8
#define MAXCOMLEN 24

#define KERNEL_LOCK() do { } while (0)
#define KERNEL_UNLOCK() do { } while (0)
#define KERNEL_ASSERT_LOCKED() do { } while (0)
#define NET_ASSERT_LOCKED() do { } while (0)
#define MUTEX_ASSERT_LOCKED(m) do { } while (0)

/* Compile-time configuration of the optional parts drivers test for. */
#define NBPFILTER 0
#define NVLAN 0
#define NKSTAT 0

/* ---- errno ---------------------------------------------------------------- */

#define EPERM 1
#define ENOENT 2
#define EIO 5
#define ENXIO 6
#define E2BIG 7
#define ENOMEM 12
#define EFAULT 14
#define EBUSY 16
#define EEXIST 17
#define ENODEV 19
#define EINVAL 22
#define ENOTTY 25
#define EFBIG 27
#define ENOSPC 28
#define ERANGE 34
#define EAGAIN 35
#define EINPROGRESS 36
#define EOPNOTSUPP 45
#define ENOBUFS 55
#define ETIMEDOUT 60
#define ENETRESET 52
#define ENETDOWN 50
#define ENOTSUP 91
#define ERESTART (-1)
#define EJUSTRETURN (-2)

/* ---- services implemented in kernel/src/bsd.rs ------------------------------ */

void *osyx_bsd_alloc(size_t size, size_t align);
void osyx_bsd_free(void *p, size_t size, size_t align);
/* Physically contiguous, zeroed, below 4 GiB; returns the physical address or 0. */
uint64_t osyx_bsd_dma_alloc(size_t size, size_t align);
void osyx_bsd_dma_free(uint64_t pa, size_t size);
uint64_t osyx_bsd_phys_to_virt(uint64_t pa);
uint64_t osyx_bsd_virt_to_phys(uint64_t va);
void osyx_bsd_log(const char *s, size_t len);
void osyx_bsd_delay_us(uint64_t us);
uint64_t osyx_bsd_ticks(void);
__dead void osyx_bsd_panic(const char *msg);
uint32_t osyx_bsd_pci_read(uint32_t tag, int reg);
void osyx_bsd_pci_write(uint32_t tag, int reg, uint32_t v);
/* Maps a memory BAR; returns its kernel virtual address or 0. */
uint64_t osyx_bsd_map_mmio(uint64_t pa, uint64_t size);
void osyx_bsd_random(void *buf, size_t len);
/* Network stack side of an attached interface. */
int osyx_bsd_ether_attach(void *ifp, const uint8_t *mac, const char *name);
void osyx_bsd_ether_input(int handle, const uint8_t *frame, size_t len);
void osyx_bsd_link_state(int handle, int up);

/* ---- libc-ish ---------------------------------------------------------------- */

void *memcpy(void *, const void *, size_t);
void *memmove(void *, const void *, size_t);
void *memset(void *, int, size_t);
int memcmp(const void *, const void *, size_t);
size_t strlen(const char *);
int strcmp(const char *, const char *);
int strncmp(const char *, const char *, size_t);
size_t strlcpy(char *, const char *, size_t);
size_t strlcat(char *, const char *, size_t);
#define bcopy(s, d, n) memmove((d), (s), (n))
#define bzero(d, n) memset((d), 0, (n))
#define bcmp(a, b, n) memcmp((a), (b), (n))

int printf(const char *, ...) __attribute__((__format__(__printf__, 1, 2)));
int snprintf(char *, size_t, const char *, ...);
int vsnprintf(char *, size_t, const char *, va_list);
int vprintf(const char *, va_list);
__dead void panic(const char *, ...);
#define KASSERT(e) do { if (__predict_false(!(e))) panic("assertion \"%s\" failed: %s:%d", #e, __FILE__, __LINE__); } while (0)
#define KASSERTMSG(e, ...) KASSERT(e)
#define CTASSERT(e) _Static_assert(e, #e)

void delay(int us);
#define DELAY(us) delay(us)
extern int hz;
extern int tick;
extern volatile int ticks;
#define SEC_TO_NSEC(s) ((uint64_t)(s) * 1000000000ULL)
#define MSEC_TO_NSEC(m) ((uint64_t)(m) * 1000000ULL)

static inline int splnet(void) { return 0; }
static inline int splhigh(void) { return 0; }
static inline int splsoftnet(void) { return 0; }
static inline void splx(int s) { (void)s; }
#define splassert(x) do { } while (0)

/* malloc(9) */
#define M_DEVBUF 2
#define M_TEMP 127
#define M_IFADDR 9
#define M_NOWAIT 0x0002
#define M_WAITOK 0x0001
#define M_ZERO 0x0008
#define M_CANFAIL 0x0004
void *malloc(size_t, int, int);
void *mallocarray(size_t, size_t, int, int);
void free(void *, int, size_t);

/* mutex(9): all driver code is serialised by the big BSD lock; the depth
 * only tells delay() that a driver waits with "interrupts blocked". */
struct mutex { int mtx_wantipl; int mtx_depth; };
#define MUTEX_INITIALIZER(ipl) { (ipl), 0 }
#define mtx_init(m, ipl) do { (m)->mtx_wantipl = (ipl); (m)->mtx_depth = 0; } while (0)
#define mtx_enter(m) do { (m)->mtx_depth++; } while (0)
#define mtx_leave(m) do { (m)->mtx_depth--; } while (0)
#define mtx_enter_try(m) ((m)->mtx_depth++, 1)

/* atomic(9) */
#define atomic_inc_int(p) (++*(p))
#define atomic_dec_int(p) (--*(p))
#define atomic_add_int(p, v) (*(p) += (v))
#define atomic_sub_int(p, v) (*(p) -= (v))
#define atomic_setbits_int(p, b) (*(p) |= (b))
#define atomic_clearbits_int(p, b) (*(p) &= ~(b))
#define membar_producer() __atomic_thread_fence(__ATOMIC_RELEASE)
#define membar_consumer() __atomic_thread_fence(__ATOMIC_ACQUIRE)
#define membar_sync() __atomic_thread_fence(__ATOMIC_SEQ_CST)

/* byte order (x86 is little endian) */
#define LITTLE_ENDIAN 1234
#define BIG_ENDIAN 4321
#define BYTE_ORDER LITTLE_ENDIAN
#define _BYTE_ORDER BYTE_ORDER
#define _LITTLE_ENDIAN LITTLE_ENDIAN
#define _BIG_ENDIAN BIG_ENDIAN
#define swap16(x) __builtin_bswap16(x)
#define swap32(x) __builtin_bswap32(x)
#define swap64(x) __builtin_bswap64(x)
#define htole16(x) ((uint16_t)(x))
#define htole32(x) ((uint32_t)(x))
#define htole64(x) ((uint64_t)(x))
#define letoh16(x) ((uint16_t)(x))
#define letoh32(x) ((uint32_t)(x))
#define letoh64(x) ((uint64_t)(x))
#define le64toh(x) letoh64(x)
#define htole64_p(p) (*(p))
#define le16toh(x) letoh16(x)
#define le32toh(x) letoh32(x)
#define htobe16(x) swap16(x)
#define htobe32(x) swap32(x)
#define htobe64(x) swap64(x)
#define betoh16(x) swap16(x)
#define betoh32(x) swap32(x)
#define betoh64(x) swap64(x)
#define htons(x) swap16(x)
#define ntohs(x) swap16(x)
#define htonl(x) swap32(x)
#define ntohl(x) swap32(x)

static inline uint8_t inb(uint16_t p) { uint8_t v; __asm__ volatile("inb %1, %0" : "=a"(v) : "Nd"(p)); return v; }
static inline uint16_t inw(uint16_t p) { uint16_t v; __asm__ volatile("inw %1, %0" : "=a"(v) : "Nd"(p)); return v; }
static inline uint32_t inl(uint16_t p) { uint32_t v; __asm__ volatile("inl %1, %0" : "=a"(v) : "Nd"(p)); return v; }
static inline void outb(uint16_t p, uint8_t v) { __asm__ volatile("outb %0, %1" : : "a"(v), "Nd"(p)); }
static inline void outw(uint16_t p, uint16_t v) { __asm__ volatile("outw %0, %1" : : "a"(v), "Nd"(p)); }
static inline void outl(uint16_t p, uint32_t v) { __asm__ volatile("outl %0, %1" : : "a"(v), "Nd"(p)); }

/* ---- timeouts and tasks --------------------------------------------------------- */

struct timeout {
	struct timeout *to_next;
	void (*to_func)(void *);
	void *to_arg;
	uint64_t to_time;
	int to_flags;
};
#define TIMEOUT_ONQUEUE 0x02
#define TIMEOUT_INITIALIZED 0x04
void timeout_set(struct timeout *, void (*)(void *), void *);
#define timeout_set_proc(to, f, a) timeout_set(to, f, a)
#define timeout_set_flags(to, f, a, k, fl) timeout_set(to, f, a)
int timeout_add(struct timeout *, int);
int timeout_add_msec(struct timeout *, int);
int timeout_add_usec(struct timeout *, int);
int timeout_add_sec(struct timeout *, int);
int timeout_del(struct timeout *);
int timeout_del_barrier(struct timeout *);
#define timeout_pending(to) (((to)->to_flags & TIMEOUT_ONQUEUE) != 0)
#define timeout_initialized(to) (((to)->to_flags & TIMEOUT_INITIALIZED) != 0)

struct task {
	struct task *t_next;
	void (*t_func)(void *);
	void *t_arg;
	int t_flags;
};
struct taskq;
extern struct taskq *const systq;
extern struct taskq *const systqmp;
#define TASK_ONQUEUE 1
void task_set(struct task *, void (*)(void *), void *);
int task_add(struct taskq *, struct task *);
int task_del(struct taskq *, struct task *);
#define task_pending(t) (((t)->t_flags & TASK_ONQUEUE) != 0)
#define taskq_barrier(tq) do { } while (0)
#define net_tq(i) systq

/* ---- autoconf ------------------------------------------------------------------- */

enum devclass { DV_DULL, DV_CPU, DV_DISK, DV_IFNET, DV_TAPE, DV_TTY };
struct cfdata;
struct device {
	enum devclass dv_class;
	struct device *dv_parent;
	int dv_unit;
	char dv_xname[16];
	struct cfdata *dv_cfdata;
	int dv_flags;
};
#define DVF_ACTIVE 1
typedef int (*cfmatch_t)(struct device *, void *, void *);
typedef int (*cfprint_t)(void *, const char *);
typedef int (*cfscan_t)(struct device *, void *);
struct cfattach {
	size_t ca_devsize;
	cfmatch_t ca_match;
	void (*ca_attach)(struct device *, struct device *, void *);
	int (*ca_detach)(struct device *, int);
	int (*ca_activate)(struct device *, int);
};
struct cfdriver {
	void **cd_devs;
	const char *cd_name;
	enum devclass cd_class;
	int cd_mode;
	int cd_ndevs;
};
struct cfdata {
	const struct cfattach *cf_attach;
	struct cfdriver *cf_driver;
	short cf_fstate;
	long *cf_loc;
	int cf_flags;
};
#define DETACH_FORCE 1
#define DETACH_QUIET 2
#define DVACT_QUIESCE 1
#define DVACT_SUSPEND 2
#define DVACT_RESUME 3
#define DVACT_WAKEUP 4
#define DVACT_POWERDOWN 5
#define UNCONF 1
#define QUIET 0
struct device *config_found_sm(struct device *, void *, cfprint_t, cfmatch_t);
#define config_found(p, a, pr) config_found_sm(p, a, pr, NULL)
int config_detach(struct device *, int);
int config_activate_children(struct device *, int);
int config_detach_children(struct device *, int);
#define CD_SKIPHIBERNATE 2
#define FREAD 0x0001
#define FWRITE 0x0002
const char *pci_findvendor(uint32_t);
#define DEVNAME(sc) ((sc)->sc_dev.dv_xname)

/* ---- bus_space / bus_dma ---------------------------------------------------------- */

typedef int bus_space_tag_t;
typedef uint64_t bus_space_handle_t;
#define X86_BUS_SPACE_IO 0
#define X86_BUS_SPACE_MEM 1
#define BUS_SPACE_BARRIER_READ 0x01
#define BUS_SPACE_BARRIER_WRITE 0x02
#define BUS_SPACE_MAP_LINEAR 0x02
#define BUS_SPACE_MAP_PREFETCHABLE 0x08

#define BUS_SPACE_RW(n, t, io_in, io_out)                                                   \
	static inline t bus_space_read_##n(bus_space_tag_t tag, bus_space_handle_t h, bus_size_t o) \
	{                                                                                       \
		if (tag == X86_BUS_SPACE_IO)                                                    \
			return io_in((uint16_t)(h + o));                                        \
		return *(volatile t *)(h + o);                                                  \
	}                                                                                       \
	static inline void bus_space_write_##n(bus_space_tag_t tag, bus_space_handle_t h, bus_size_t o, t v) \
	{                                                                                       \
		if (tag == X86_BUS_SPACE_IO)                                                    \
			io_out((uint16_t)(h + o), v);                                           \
		else                                                                            \
			*(volatile t *)(h + o) = v;                                             \
	}
BUS_SPACE_RW(1, uint8_t, inb, outb)
BUS_SPACE_RW(2, uint16_t, inw, outw)
BUS_SPACE_RW(4, uint32_t, inl, outl)
#undef BUS_SPACE_RW
static inline void bus_space_barrier(bus_space_tag_t t, bus_space_handle_t h, bus_size_t o, bus_size_t l, int f)
{
	(void)t; (void)h; (void)o; (void)l; (void)f;
	__atomic_thread_fence(__ATOMIC_SEQ_CST);
}
/* Raw regions: bytes in memory order, `len` a multiple of the access size. */
static inline void bus_space_write_raw_region_4(bus_space_tag_t t, bus_space_handle_t h, bus_size_t o, const void *p, bus_size_t len)
{
	for (bus_size_t i = 0; i < len; i += 4) {
		uint32_t v;
		memcpy(&v, (const uint8_t *)p + i, 4);
		bus_space_write_4(t, h, o + i, v);
	}
}
static inline void bus_space_read_raw_region_4(bus_space_tag_t t, bus_space_handle_t h, bus_size_t o, void *p, bus_size_t len)
{
	for (bus_size_t i = 0; i < len; i += 4) {
		uint32_t v = bus_space_read_4(t, h, o + i);
		memcpy((uint8_t *)p + i, &v, 4);
	}
}
static inline void bus_space_unmap(bus_space_tag_t t, bus_space_handle_t h, bus_size_t s) { (void)t; (void)h; (void)s; }

typedef struct osyx_dma_tag { int dt_dummy; } *bus_dma_tag_t;
typedef struct {
	bus_addr_t ds_addr;
	bus_size_t ds_len;
} bus_dma_segment_t;
struct bus_dmamap {
	bus_size_t dm_size;
	int dm_maxsegs;
	bus_size_t dm_maxsegsz;
	bus_size_t dm_mapsize;
	int dm_nsegs;
	bus_dma_segment_t dm_segs[1];
};
typedef struct bus_dmamap *bus_dmamap_t;
#define BUS_DMA_WAITOK 0x0000
#define BUS_DMA_NOWAIT 0x0001
#define BUS_DMA_ALLOCNOW 0x0002
#define BUS_DMA_COHERENT 0x0004
#define BUS_DMA_STREAMING 0x0008
#define BUS_DMA_READ 0x0200
#define BUS_DMA_WRITE 0x0400
#define BUS_DMA_ZERO 0x1000
#define BUS_DMA_64BIT 0x2000
#define BUS_DMA_OVERRUN 0x4000
#define BUS_DMASYNC_PREREAD 0x01
#define BUS_DMASYNC_POSTREAD 0x02
#define BUS_DMASYNC_PREWRITE 0x04
#define BUS_DMASYNC_POSTWRITE 0x08
struct mbuf;
struct proc;
int bus_dmamap_create(bus_dma_tag_t, bus_size_t, int, bus_size_t, bus_size_t, int, bus_dmamap_t *);
void bus_dmamap_destroy(bus_dma_tag_t, bus_dmamap_t);
int bus_dmamap_load(bus_dma_tag_t, bus_dmamap_t, void *, bus_size_t, struct proc *, int);
int bus_dmamap_load_mbuf(bus_dma_tag_t, bus_dmamap_t, struct mbuf *, int);
void bus_dmamap_unload(bus_dma_tag_t, bus_dmamap_t);
static inline void bus_dmamap_sync(bus_dma_tag_t t, bus_dmamap_t m, bus_addr_t o, bus_size_t l, int ops)
{
	(void)t; (void)m; (void)o; (void)l; (void)ops;
	__atomic_thread_fence(__ATOMIC_SEQ_CST);
}
int bus_dmamem_alloc(bus_dma_tag_t, bus_size_t, bus_size_t, bus_size_t, bus_dma_segment_t *, int, int *, int);
void bus_dmamem_free(bus_dma_tag_t, bus_dma_segment_t *, int);
int bus_dmamem_map(bus_dma_tag_t, bus_dma_segment_t *, int, size_t, caddr_t *, int);
void bus_dmamem_unmap(bus_dma_tag_t, caddr_t, size_t);

/* ---- mbufs ------------------------------------------------------------------------- */

#define MSIZE 256
#define MLEN 224
#define MHLEN 200
#define MCLBYTES 2048
#define MCLSHIFT 11
#define MAXMCLBYTES (64 * 1024)
#define MT_DATA 1
#define MT_HEADER 2
#define M_EXT 0x0001
#define M_PKTHDR 0x0002
#define M_EOR 0x0004
#define M_BCAST 0x0100
#define M_MCAST 0x0200
#define M_VLANTAG 0x0020
#define M_DONTWAIT M_NOWAIT
#define M_WAIT M_WAITOK
/* checksum offload flags */
#define M_IPV4_CSUM_OUT 0x0001
#define M_TCP_CSUM_OUT 0x0002
#define M_UDP_CSUM_OUT 0x0004
#define M_IPV4_CSUM_IN_OK 0x0008
#define M_IPV4_CSUM_IN_BAD 0x0010
#define M_TCP_CSUM_IN_OK 0x0020
#define M_TCP_CSUM_IN_BAD 0x0040
#define M_UDP_CSUM_IN_OK 0x0080
#define M_UDP_CSUM_IN_BAD 0x0100
#define M_ICMP_CSUM_OUT 0x0200
#define M_ICMP_CSUM_IN_OK 0x0400
#define M_ICMP_CSUM_IN_BAD 0x0800
#define M_IPV6_DF_OUT 0x1000
#define M_TIMESTAMP 0x2000
#define M_FLOWID 0x4000

struct pkthdr {
	void *ph_cookie;
	int len;
	uint16_t csum_flags;
	uint16_t ether_vtag;
	unsigned int ph_ifidx;
	uint16_t ph_flowid;
};
/* Every mbuf owns one physically contiguous buffer (DMA-able as one segment). */
struct mbuf {
	struct mbuf *m_next;
	struct mbuf *m_nextpkt;
	caddr_t m_data;
	unsigned int m_len;
	short m_type;
	unsigned short m_flags;
	struct pkthdr m_pkthdr;
	struct {
		caddr_t ext_buf;
		unsigned int ext_size;
	} m_ext;
	uint64_t m_bufphys;
	size_t m_bufsize;
};
#define mtod(m, t) ((t)((m)->m_data))
#define M_LEADINGSPACE(m) ((size_t)((m)->m_data - (m)->m_ext.ext_buf))
#define M_TRAILINGSPACE(m) ((size_t)((m)->m_ext.ext_buf + (m)->m_ext.ext_size - ((m)->m_data + (m)->m_len)))
#define M_ALIGN(m, len) ((m)->m_data += (MLEN - (len)) & ~(sizeof(long) - 1))
#define MH_ALIGN(m, len) ((m)->m_data += (MHLEN - (len)) & ~(sizeof(long) - 1))
struct mbuf *m_gethdr(int, int);
struct mbuf *m_get(int, int);
struct mbuf *m_clget(struct mbuf *, int, unsigned int);
#define MGETHDR(m, how, type) ((m) = m_gethdr((how), (type)))
#define MGET(m, how, type) ((m) = m_get((how), (type)))
#define MCLGET(m, how) (void)m_clget((m), (how), MCLBYTES)
#define MCLGETL(m, how, size) m_clget((m), (how), (size))
struct mbuf *m_free(struct mbuf *);
void m_freem(struct mbuf *);
void m_adj(struct mbuf *, int);
void m_copydata(struct mbuf *, int, int, void *);
int m_copyback(struct mbuf *, int, int, const void *, int);
struct mbuf *m_devget(char *, int, int);
struct mbuf *m_getptr(struct mbuf *, int, int *);
int m_defrag(struct mbuf *, int);
struct mbuf *m_pullup(struct mbuf *, int);
struct mbuf *m_prepend(struct mbuf *, int, int);
#define M_PREPEND(m, plen, how) ((m) = m_prepend((m), (plen), (how)))

struct mbuf_list {
	struct mbuf *ml_head;
	struct mbuf *ml_tail;
	unsigned int ml_len;
};
#define MBUF_LIST_INITIALIZER() { NULL, NULL, 0 }
void ml_init(struct mbuf_list *);
void ml_enqueue(struct mbuf_list *, struct mbuf *);
struct mbuf *ml_dequeue(struct mbuf_list *);
unsigned int ml_purge(struct mbuf_list *);
#define ml_len(ml) ((ml)->ml_len)
#define ml_empty(ml) ((ml)->ml_len == 0)

struct mbuf_queue {
	struct mutex mq_mtx;
	struct mbuf_list mq_list;
	unsigned int mq_maxlen;
	unsigned int mq_drops;
};
#define MBUF_QUEUE_INITIALIZER(len, ipl) { { (ipl) }, MBUF_LIST_INITIALIZER(), (len), 0 }
void mq_init(struct mbuf_queue *, unsigned int, int);
int mq_enqueue(struct mbuf_queue *, struct mbuf *);
struct mbuf *mq_dequeue(struct mbuf_queue *);
unsigned int mq_purge(struct mbuf_queue *);
void mq_delist(struct mbuf_queue *, struct mbuf_list *);
#define mq_len(mq) ((mq)->mq_list.ml_len)
#define mq_empty(mq) (mq_len(mq) == 0)
#define mq_full(mq) (mq_len(mq) >= (mq)->mq_maxlen)
#define mq_drops(mq) ((mq)->mq_drops)
#define mq_set_maxlen(mq, l) ((mq)->mq_maxlen = (l))
#define MINCLSIZE (MHLEN + 1)
#define M_COPYALL 1000000000
/* Packet tags: none are ever attached in OSyx. */
struct m_tag { uint16_t m_tag_id; uint16_t m_tag_len; struct m_tag *m_tag_next; };
#define PACKET_TAG_DLT 0x0100
#define m_tag_find(m, t, p) ((struct m_tag *)NULL)
struct mbuf *m_dup_pkt(struct mbuf *, unsigned int, int);
int m_dup_pkthdr(struct mbuf *, struct mbuf *, int);
struct mbuf *m_copym(struct mbuf *, int, int, int);
struct mbuf *m_split(struct mbuf *, int, int);
struct mbuf *m_makespace(struct mbuf *, int, int, int *);
void m_cat(struct mbuf *, struct mbuf *);
int m_leadingspace(struct mbuf *);
int m_trailingspace(struct mbuf *);
void m_purge(struct mbuf *);
void m_align(struct mbuf *, int);

/* ---- sockets, ioctls ------------------------------------------------------------------ */

struct sockaddr {
	uint8_t sa_len;
	sa_family_t sa_family;
	char sa_data[14];
};
#define AF_UNSPEC 0
#define AF_INET 2
#define AF_LINK 18
struct in_addr { in_addr_t s_addr; };
struct sockaddr_in {
	uint8_t sin_len;
	sa_family_t sin_family;
	uint16_t sin_port;
	struct in_addr sin_addr;
	int8_t sin_zero[8];
};
#define _IOC(inout, group, num, len) ((unsigned long)((inout) | (((len) & 0x1fff) << 16) | ((group) << 8) | (num)))
#define IOC_OUT 0x40000000UL
#define IOC_IN 0x80000000UL
#define IOC_INOUT (IOC_IN | IOC_OUT)
#define _IOW(g, n, t) _IOC(IOC_IN, (g), (n), sizeof(t))
#define _IOR(g, n, t) _IOC(IOC_OUT, (g), (n), sizeof(t))
#define _IOWR(g, n, t) _IOC(IOC_INOUT, (g), (n), sizeof(t))
struct ifreq;
struct ifmediareq;
struct if_rxrinfo;
#define SIOCSIFADDR _IOW('i', 12, struct ifreq)
#define SIOCSIFFLAGS _IOW('i', 16, struct ifreq)
#define SIOCADDMULTI _IOW('i', 49, struct ifreq)
#define SIOCDELMULTI _IOW('i', 50, struct ifreq)
#define SIOCSIFMTU _IOW('i', 127, struct ifreq)
#define SIOCSIFMEDIA _IOWR('i', 55, struct ifreq)
#define SIOCGIFMEDIA _IOWR('i', 56, struct ifmediareq)
#define SIOCGIFRXR _IOW('i', 170, struct ifreq)
#define SIOCGIFSFFPAGE _IOWR('i', 57, struct ifreq)
int copyout(const void *, void *, size_t);
int copyin(const void *, void *, size_t);

/* ---- network interfaces ------------------------------------------------------------- */

#define IFNAMSIZ 16
#define ETHER_ADDR_LEN 6
#define ETHER_TYPE_LEN 2
#define ETHER_CRC_LEN 4
#define ETHER_HDR_LEN (ETHER_ADDR_LEN * 2 + ETHER_TYPE_LEN)
#define ETHER_MIN_LEN 64
#define ETHER_MAX_LEN 1518
#define ETHER_MAX_DIX_LEN 1536
#define ETHER_VLAN_ENCAP_LEN 4
#define ETHERMTU (ETHER_MAX_LEN - ETHER_HDR_LEN - ETHER_CRC_LEN)
#define ETHERMIN (ETHER_MIN_LEN - ETHER_HDR_LEN - ETHER_CRC_LEN)
#define ETHER_ALIGN 2
#define ETHERTYPE_IP 0x0800
#define ETHERTYPE_IPV6 0x86DD
#define ETHERTYPE_ARP 0x0806
#define ETHERTYPE_EAPOL 0x888E
#define ETHERTYPE_PAE ETHERTYPE_EAPOL
#define ETHER_IS_EQ(a, b) (memcmp((a), (b), ETHER_ADDR_LEN) == 0)
#define ETHERTYPE_VLAN 0x8100
#define ETHER_IS_MULTICAST(a) ((a)[0] & 0x01)
struct ether_header {
	uint8_t ether_dhost[ETHER_ADDR_LEN];
	uint8_t ether_shost[ETHER_ADDR_LEN];
	uint16_t ether_type;
} __packed;
struct ether_addr { uint8_t ether_addr_octet[ETHER_ADDR_LEN]; };
extern const uint8_t etherbroadcastaddr[ETHER_ADDR_LEN];
extern const uint8_t etheranyaddr[ETHER_ADDR_LEN];
char *ether_sprintf(uint8_t *);
uint32_t ether_crc32_le(const uint8_t *, size_t);
uint32_t ether_crc32_be(const uint8_t *, size_t);
#define IPTOS_DSCP_CS0 0x00
#define IPTOS_DSCP_CS1 0x20
#define IPTOS_DSCP_AF11 0x28
#define IPTOS_DSCP_AF12 0x30
#define IPTOS_DSCP_AF13 0x38
#define IPTOS_DSCP_CS2 0x40
#define IPTOS_DSCP_AF21 0x48
#define IPTOS_DSCP_AF22 0x50
#define IPTOS_DSCP_AF23 0x58
#define IPTOS_DSCP_CS3 0x60
#define IPTOS_DSCP_AF31 0x68
#define IPTOS_DSCP_AF32 0x70
#define IPTOS_DSCP_AF33 0x78
#define IPTOS_DSCP_CS4 0x80
#define IPTOS_DSCP_AF41 0x88
#define IPTOS_DSCP_AF42 0x90
#define IPTOS_DSCP_AF43 0x98
#define IPTOS_DSCP_CS5 0xa0
#define IPTOS_DSCP_VA 0xb0
#define IPTOS_DSCP_EF 0xb8
#define IPTOS_DSCP_CS6 0xc0
#define IPTOS_DSCP_CS7 0xe0
#define IPTOS_DSCP_MASK 0xfc
struct ip {
	unsigned int ip_hl : 4, ip_v : 4;
	uint8_t ip_tos;
	uint16_t ip_len;
	uint16_t ip_id;
	uint16_t ip_off;
	uint8_t ip_ttl;
	uint8_t ip_p;
	uint16_t ip_sum;
	struct in_addr ip_src, ip_dst;
} __packed;
int in_cksum(struct mbuf *, int);
void in_delayed_cksum(struct mbuf *);
/* Firmware strings from SMBIOS (not read by OSyx). */
extern char *hw_vendor, *hw_prod;

#define IFF_UP 0x1
#define IFF_BROADCAST 0x2
#define IFF_DEBUG 0x4
#define IFF_LOOPBACK 0x8
#define IFF_POINTOPOINT 0x10
#define IFF_STATICARP 0x20
#define IFF_RUNNING 0x40
#define IFF_NOARP 0x80
#define IFF_PROMISC 0x100
#define IFF_ALLMULTI 0x200
#define IFF_OACTIVE 0x400
#define IFF_SIMPLEX 0x800
#define IFF_LINK0 0x1000
#define IFF_LINK1 0x2000
#define IFF_LINK2 0x4000
#define IFF_MULTICAST 0x8000
#define IFXF_MPSAFE 0x1
#define IFXF_WOL 0x10
#define IFXF_MBUF_64BIT 0x40
#define IFCAP_CSUM_IPv4 0x00000001
#define IFCAP_CSUM_TCPv4 0x00000002
#define IFCAP_CSUM_UDPv4 0x00000004
#define IFCAP_VLAN_MTU 0x00000010
#define IFCAP_VLAN_HWTAGGING 0x00000020
#define IFCAP_CSUM_TCPv6 0x00000080
#define IFCAP_CSUM_UDPv6 0x00000100
#define IFCAP_TSOv4 0x00001000
#define IFCAP_TSOv6 0x00002000
#define IFCAP_LRO 0x00004000
#define IFCAP_WOL 0x00008000
#define IFCAP_CSUM_IP IFCAP_CSUM_IPv4
#define IFCAP_CSUM_TCP IFCAP_CSUM_TCPv4
#define IFCAP_CSUM_UDP IFCAP_CSUM_UDPv4

#define LINK_STATE_UNKNOWN 0
#define LINK_STATE_INVALID 1
#define LINK_STATE_DOWN 2
#define LINK_STATE_KALIVE_DOWN 3
#define LINK_STATE_UP 4
#define LINK_STATE_HALF_DUPLEX 5
#define LINK_STATE_FULL_DUPLEX 6
#define LINK_STATE_IS_UP(s) ((s) >= LINK_STATE_UP || (s) == LINK_STATE_UNKNOWN)

#define IF_Kbps(x) ((uint64_t)(x) * 1000ULL)
#define IF_Mbps(x) (IF_Kbps((x) * 1000ULL))
#define IF_Gbps(x) (IF_Mbps((x) * 1000ULL))

struct ifnet;
struct ifqueue {
	struct ifnet *ifq_if;
	struct mbuf_list ifq_list;
	unsigned int ifq_maxlen;
	int ifq_oactive;
	uint64_t ifq_qdrops;
};
int ifq_enqueue(struct ifqueue *, struct mbuf *);
struct mbuf *ifq_dequeue(struct ifqueue *);
struct mbuf *ifq_deq_begin(struct ifqueue *);
void ifq_deq_commit(struct ifqueue *, struct mbuf *);
void ifq_deq_rollback(struct ifqueue *, struct mbuf *);
unsigned int ifq_purge(struct ifqueue *);
#define ifq_len(q) ((q)->ifq_list.ml_len)
#define ifq_empty(q) (ifq_len(q) == 0)
#define ifq_is_oactive(q) ((q)->ifq_oactive)
#define ifq_set_oactive(q) ((q)->ifq_oactive = 1)
#define ifq_clr_oactive(q) ((q)->ifq_oactive = 0)
#define ifq_set_maxlen(q, n) ((q)->ifq_maxlen = (n))
#define ifq_init_maxlen(q, n) ifq_set_maxlen(q, n)
#define IFQ_SET_MAXLEN(q, n) ifq_set_maxlen(q, n)
#define ifq_if(q) ((q)->ifq_if)
#define ifq_barrier(q) do { } while (0)
void ifq_restart(struct ifqueue *);
void ifq_serialize(struct ifqueue *, struct task *);

struct if_data {
	uint8_t ifi_type;
	uint8_t ifi_addrlen;
	uint8_t ifi_hdrlen;
	uint8_t ifi_link_state;
	uint32_t ifi_mtu;
	uint32_t ifi_metric;
	uint32_t ifi_rdomain;
	uint64_t ifi_baudrate;
	uint64_t ifi_ipackets;
	uint64_t ifi_ierrors;
	uint64_t ifi_opackets;
	uint64_t ifi_oerrors;
	uint64_t ifi_collisions;
	uint64_t ifi_ibytes;
	uint64_t ifi_obytes;
	uint64_t ifi_iqdrops;
	uint64_t ifi_oqdrops;
	uint64_t ifi_imcasts;
	uint64_t ifi_omcasts;
};
struct ifiqueue { struct ifnet *ifiq_if; };
int ifiq_input(struct ifiqueue *, struct mbuf_list *);
struct ifnet {
	void *if_softc;
	char if_xname[IFNAMSIZ];
	int if_pcount;
	int if_timer;
	unsigned short if_flags;
	int if_xflags;
	uint32_t if_hardmtu;
	uint32_t if_capabilities;
	struct if_data if_data;
	struct ifqueue if_snd;
	struct ifiqueue if_rcv;
	struct ifqueue *if_ifqs[1];
	int (*if_ioctl)(struct ifnet *, unsigned long, caddr_t);
	void (*if_start)(struct ifnet *);
	void (*if_qstart)(struct ifqueue *);
	void (*if_watchdog)(struct ifnet *);
	int (*if_wol)(struct ifnet *, int);
	int (*if_output)(struct ifnet *, struct mbuf *, struct sockaddr *, struct rtentry *);
	uint8_t if_priority;
	/* OSyx */
	int if_osyx_handle;
};
#define if_mtu if_data.ifi_mtu
#define if_type if_data.ifi_type
#define if_link_state if_data.ifi_link_state
#define if_baudrate if_data.ifi_baudrate
#define if_ipackets if_data.ifi_ipackets
#define if_ierrors if_data.ifi_ierrors
#define if_opackets if_data.ifi_opackets
#define if_oerrors if_data.ifi_oerrors
#define if_collisions if_data.ifi_collisions
#define if_iqdrops if_data.ifi_iqdrops
#define if_oqdrops if_data.ifi_oqdrops
#define if_ibytes if_data.ifi_ibytes
#define if_hdrlen if_data.ifi_hdrlen
#define if_addrlen if_data.ifi_addrlen
#define if_obytes if_data.ifi_obytes
#define if_imcasts if_data.ifi_imcasts
#define if_omcasts if_data.ifi_omcasts
#define IF_WIRELESS_DEFAULT_PRIORITY 4
#define IFQ_MAXLEN 256
#define DLT_EN10MB 1
#define DLT_IEEE802_11 105
#define DLT_IEEE802_11_RADIO 127
void if_start(struct ifnet *);
struct if_ieee80211_data {
	uint8_t ifie_channel;
	uint8_t ifie_nwid_len;
	uint32_t ifie_flags;
	int32_t ifie_xflags;
	uint8_t ifie_nwid[32];
	uint8_t ifie_addr[6];
};
#define rtm_80211info(ifp, ifie) do { (void)(ifp); (void)(ifie); } while (0)
int if_setlladdr(struct ifnet *, const uint8_t *);
struct rtentry;
void if_deactivate(struct ifnet *);
void if_addgroup(struct ifnet *, const char *);
int if_enqueue(struct ifnet *, struct mbuf *);
int ether_output(struct ifnet *, struct mbuf *, struct sockaddr *, struct rtentry *);
#define IFT_ETHER 0x06

struct ifreq {
	char ifr_name[IFNAMSIZ];
	union {
		struct sockaddr ifru_addr;
		short ifru_flags;
		int ifru_metric;
		int ifru_mtu;
		uint64_t ifru_media;
		caddr_t ifru_data;
	} ifr_ifru;
};
#define ifr_addr ifr_ifru.ifru_addr
#define ifr_flags ifr_ifru.ifru_flags
#define ifr_mtu ifr_ifru.ifru_mtu
#define ifr_media ifr_ifru.ifru_media
#define ifr_data ifr_ifru.ifru_data
struct ifaddr { struct sockaddr *ifa_addr; };
struct ifmediareq {
	char ifm_name[IFNAMSIZ];
	uint64_t ifm_current;
	uint64_t ifm_mask;
	uint64_t ifm_status;
	uint64_t ifm_active;
	int ifm_count;
	uint64_t *ifm_ulist;
};

struct ether_multi {
	uint8_t enm_addrlo[ETHER_ADDR_LEN];
	uint8_t enm_addrhi[ETHER_ADDR_LEN];
};
struct ether_multistep { struct ether_multi *e_enm; };
struct arpcom {
	struct ifnet ac_if;
	uint8_t ac_enaddr[ETHER_ADDR_LEN];
	int ac_multicnt;
	int ac_multirangecnt;
};
int ether_addmulti(struct ifreq *, struct arpcom *);
int ether_delmulti(struct ifreq *, struct arpcom *);
/* OSyx keeps no multicast list in the driver: the filter accepts all groups. */
#define ETHER_FIRST_MULTI(step, ac, enm) do { (step).e_enm = NULL; (enm) = NULL; (void)(ac); } while (0)
#define ETHER_NEXT_MULTI(step, enm) do { (enm) = NULL; } while (0)

void if_attach(struct ifnet *);
void ether_ifattach(struct ifnet *);
void if_detach(struct ifnet *);
void ether_ifdetach(struct ifnet *);
void if_input(struct ifnet *, struct mbuf_list *);
void if_link_state_change(struct ifnet *);
int ether_ioctl(struct ifnet *, struct arpcom *, unsigned long, caddr_t);
void if_counters_alloc(struct ifnet *);
#define if_attach_queues(ifp, n) do { } while (0)
#define if_attach_iqueues(ifp, n) do { } while (0)

struct if_rxring {
	int rxr_adjusted;
	unsigned int rxr_alive;
	unsigned int rxr_cwm;
	unsigned int rxr_lwm;
	unsigned int rxr_hwm;
};
struct if_rxring_info { char ifr_name[16]; unsigned int ifr_size; struct if_rxring ifr_info; };
void if_rxr_init(struct if_rxring *, unsigned int, unsigned int);
unsigned int if_rxr_get(struct if_rxring *, unsigned int);
#define if_rxr_put(r, n) do { (r)->rxr_alive -= (n); } while (0)
#define if_rxr_inuse(r) ((r)->rxr_alive)
#define if_rxr_cwm(r) ((r)->rxr_cwm)
#define if_rxr_livelocked(r) do { } while (0)
int if_rxr_ioctl(struct if_rxrinfo *, const char *, unsigned int, struct if_rxring *);

/* ---- PCI ---------------------------------------------------------------------------------- */

typedef uint32_t pcireg_t;
typedef uint32_t pcitag_t;
typedef void *pci_chipset_tag_t;
typedef struct { int ih_msi; } pci_intr_handle_t;
struct pci_attach_args {
	bus_space_tag_t pa_iot;
	bus_space_tag_t pa_memt;
	bus_dma_tag_t pa_dmat;
	bus_dma_tag_t pa_dmat64;
	pci_chipset_tag_t pa_pc;
	int pa_flags;
	unsigned int pa_device;
	unsigned int pa_bus;
	unsigned int pa_function;
	pcitag_t pa_tag;
	pcireg_t pa_id;
	pcireg_t pa_class;
	unsigned int pa_intrpin;
	int pa_intrline;
};
#define PCI_FLAGS_IO_ENABLED 0x01
#define PCI_FLAGS_MEM_ENABLED 0x02
#define PCI_FLAGS_MSI_ENABLED 0x20
#define PCI_FLAGS_MSIVEC_ENABLED 0x40
struct pci_matchid {
	uint16_t pm_vid;
	uint16_t pm_pid;
};
int pci_matchbyid(struct pci_attach_args *, const struct pci_matchid *, int);
pcireg_t pci_conf_read(pci_chipset_tag_t, pcitag_t, int);
void pci_conf_write(pci_chipset_tag_t, pcitag_t, int, pcireg_t);
int pci_mapreg_map(struct pci_attach_args *, int, pcireg_t, int, bus_space_tag_t *, bus_space_handle_t *, bus_addr_t *, bus_size_t *, bus_size_t);
pcireg_t pci_mapreg_type(pci_chipset_tag_t, pcitag_t, int);
int pci_get_capability(pci_chipset_tag_t, pcitag_t, int, int *, pcireg_t *);
int pci_set_powerstate(pci_chipset_tag_t, pcitag_t, int);
int pci_intr_map(struct pci_attach_args *, pci_intr_handle_t *);
int pci_intr_map_msi(struct pci_attach_args *, pci_intr_handle_t *);
int pci_intr_map_msix(struct pci_attach_args *, int, pci_intr_handle_t *);
const char *pci_intr_string(pci_chipset_tag_t, pci_intr_handle_t);
void *pci_intr_establish(pci_chipset_tag_t, pci_intr_handle_t, int, int (*)(void *), void *, const char *);
void pci_intr_disestablish(pci_chipset_tag_t, void *);
#define intr_barrier(ih) do { (void)(ih); } while (0)
#define IPL_NET 0x7
#define IPL_AUDIO 0x8
#define IPL_MPSAFE 0x100
#define IPL_NONE 0

/* ---- misc kernel interfaces used by net80211 and iwm ---------------------------------- */

void explicit_bzero(void *, size_t);
int timingsafe_bcmp(const void *, const void *, size_t);
uint32_t arc4random(void);
void arc4random_buf(void *, size_t);
uint32_t arc4random_uniform(uint32_t);
uint32_t ether_crc32_le_update(uint32_t, const uint8_t *, size_t);
uint32_t ether_crc32_be_update(uint32_t, const uint8_t *, size_t);

#define setbit(a, i) ((a)[(i) / NBBY] |= 1 << ((i) % NBBY))
#define clrbit(a, i) ((a)[(i) / NBBY] &= ~(1 << ((i) % NBBY)))
#define isset(a, i) ((a)[(i) / NBBY] & (1 << ((i) % NBBY)))
#define isclr(a, i) (((a)[(i) / NBBY] & (1 << ((i) % NBBY))) == 0)

struct timeval { time_t tv_sec; long tv_usec; };
struct timespec { time_t tv_sec; long tv_nsec; };
#define timerisset(t) ((t)->tv_sec || (t)->tv_usec)
#define timerclear(t) ((t)->tv_sec = (t)->tv_usec = 0)
#define timercmp(a, b, op) (((a)->tv_sec == (b)->tv_sec) ? ((a)->tv_usec op (b)->tv_usec) : ((a)->tv_sec op (b)->tv_sec))
#define timeradd(a, b, r) do { (r)->tv_sec = (a)->tv_sec + (b)->tv_sec; (r)->tv_usec = (a)->tv_usec + (b)->tv_usec; \
	if ((r)->tv_usec >= 1000000) { (r)->tv_sec++; (r)->tv_usec -= 1000000; } } while (0)
#define timersub(a, b, r) do { (r)->tv_sec = (a)->tv_sec - (b)->tv_sec; (r)->tv_usec = (a)->tv_usec - (b)->tv_usec; \
	if ((r)->tv_usec < 0) { (r)->tv_sec--; (r)->tv_usec += 1000000; } } while (0)
#define USEC_TO_TIMEVAL(us, tv) do { (tv)->tv_sec = (us) / 1000000; (tv)->tv_usec = (us) % 1000000; } while (0)
void microuptime(struct timeval *);
void getmicrouptime(struct timeval *);
void microtime(struct timeval *);
void getmicrotime(struct timeval *);
void nanouptime(struct timespec *);
void getnanouptime(struct timespec *);
time_t gettime(void);
time_t getuptime(void);
#define time_uptime getuptime()
#define time_second gettime()
int ratecheck(struct timeval *, const struct timeval *);
uint64_t nsecuptime(void);

extern int cold;
static inline int abs(int x) { return x < 0 ? -x : x; }
/* x86 accesses unaligned data fine. */
#define ALIGNED_POINTER(p, t) 1
struct taskq *taskq_create(const char *, unsigned int, int, unsigned int);
void taskq_destroy(struct taskq *);
#define TASKQ_MPSAFE 1

/* No user processes reach these paths: the caller is always privileged. */
#define curproc ((struct proc *)NULL)
#define suser(p) 0
#define securelevel 0

/* syslog(9) */
#define LOG_EMERG 0
#define LOG_ALERT 1
#define LOG_CRIT 2
#define LOG_ERR 3
#define LOG_WARNING 4
#define LOG_NOTICE 5
#define LOG_INFO 6
#define LOG_DEBUG 7
void log(int, const char *, ...) __attribute__((__format__(__printf__, 2, 3)));
void addlog(const char *, ...);

/* refcnt(9) */
struct refcnt { unsigned int r_refs; };
#define REFCNT_INITIALIZER() { 1 }
#define refcnt_init(r) ((r)->r_refs = 1)
#define refcnt_take(r) ((r)->r_refs++)
#define refcnt_rele(r) (--(r)->r_refs == 0)
#define refcnt_rele_wake(r) do { (r)->r_refs--; } while (0)
#define refcnt_finalize(r, w) do { (r)->r_refs--; } while (0)
#define refcnt_shared(r) ((r)->r_refs > 1)
#define refcnt_read(r) ((r)->r_refs)

/* rwlock(9): serialised by the big lock. */
struct rwlock { const char *rwl_name; int rwl_owner; };
#define RWLOCK_INITIALIZER(n) { (n), 0 }
#define rw_init(l, n) do { (l)->rwl_name = (n); (l)->rwl_owner = 0; } while (0)
#define rw_enter_write(l) ((l)->rwl_owner = 1)
#define rw_exit_write(l) ((l)->rwl_owner = 0)
#define rw_enter_read(l) do { (void)(l); } while (0)
#define rw_exit_read(l) do { (void)(l); } while (0)
#define rw_exit(l) ((l)->rwl_owner = 0)
#define RW_WRITE 0x01
#define RW_READ 0x02
#define RW_INTR 0x10
#define RW_NOSLEEP 0x40
static inline int rw_enter(struct rwlock *l, int f) { (void)f; l->rwl_owner = 1; return 0; }
#define rw_status(l) ((l)->rwl_owner ? RW_WRITE : 0)
#define rw_assert_wrlock(l) do { } while (0)

/* Sleeping: the big lock stays held, and the sleeper itself runs the
 * polled interrupt handlers until wakeup(9) or the timeout. */
#define PCATCH 0x100
#define PWAIT 32
#define PZERO 22
#define EWOULDBLOCK EAGAIN
#define EINTR 4
int tsleep_nsec(const volatile void *, int, const char *, uint64_t);
#define tsleep(id, pri, msg, t) tsleep_nsec((id), (pri), (msg), (t) <= 0 ? INFSLP : MSEC_TO_NSEC((uint64_t)(t) * 1000 / hz))
void wakeup(const volatile void *);
void wakeup_one(const volatile void *);
#define msleep_nsec(id, mtx, pri, msg, t) tsleep_nsec((id), (pri), (msg), (t))
#define INFSLP UINT64_MAX
#define USEC_TO_NSEC(u) ((uint64_t)(u) * 1000ULL)

/* firmware(9): files under /etc/firmware on the OSyx root. */
int loadfirmware(const char *, uint8_t **, size_t *);
/* Deferred until the root file system is mounted (firmware lives there). */
void config_mountroot(struct device *, void (*)(struct device *));
struct proc;
int kthread_create(void (*)(void *), void *, struct proc **, const char *);

/* kstat placeholders (NKSTAT is 0) */
struct kstat;

#endif /* OSYX_BSD_H */
