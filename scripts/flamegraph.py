#!/usr/bin/env python3
"""Flame graphs and a report from an OSyx kernel-profiler log.

Input: a serial log of a boot with `prof=<seconds>` (scripts/zen-test.ps1 -Prof,
kernel/src/prof.rs). Output, in the given directory:

  cpu.svg / cpu.folded         on-CPU time: user code and kernel code, 1 ms per sample
  offcpu.svg / offcpu.folded   threads blocked in the kernel, with the kernel stack
                               and last system call, 20 ms per visit
  runq.svg / runq.folded       threads runnable but without a CPU
  report.md                    totals, hottest functions, hottest xul.dll addresses
  index.html                   the three graphs on one page

Kernel addresses are named from the kernel ELF's symbol table, user addresses
from the module bases winrun logged (`winrun: module NAME base= size=`). The
numbers are samples, not instruction counts; nothing here is simulated.
"""
import argparse, bisect, collections, html, os, re, struct, sys

KERNEL_BASE = 0xFFFFFFFF80000000


def read_symbols(path, lo=0):
    """(addr, size, name) sorted, from `nm -C` (GNU binutils; through WSL on Windows).
    nm demangles Rust's v0 and legacy schemes, which this script does not."""
    import shutil, subprocess
    if shutil.which('nm'):
        cmd = ['nm', '-C', '-n', '-S', '--defined-only', path]
    else:
        p = os.path.abspath(path).replace(chr(92), '/')
        wsl = '/mnt/' + p[0].lower() + p[2:]
        cmd = ['wsl.exe', '-d', 'Ubuntu-24.04', '--', 'nm', '-C', '-n', '-S', '--defined-only', wsl]
    out = subprocess.run(cmd, capture_output=True, check=True).stdout.decode('utf8', 'replace')
    syms = []
    for ln in out.splitlines():
        m = re.match(r'([0-9a-f]+) (?:([0-9a-f]+) )?([A-Za-z]) (.*)$', ln)
        if m and m.group(3) in 'tTwW' and int(m.group(1), 16) >= lo:
            syms.append((int(m.group(1), 16), int(m.group(2) or '0', 16), m.group(4)))
    syms.sort()
    return syms


def shorten(name, limit=80):
    """Collapse generic arguments until the name is readable."""
    m = re.match(r'<(.*) as (.*)>::(\w+)$', name)
    if m and not name.startswith('<<'):
        name = '%s::%s' % (m.group(1), m.group(3))
    name = name.replace('core::ptr::drop_glue::', 'drop ').replace('osyx_kernel::', '').replace('alloc::', '').replace('core::', '')
    while len(name) > limit:
        n = re.sub(r'<[^<>]*>', '<>', name, count=1)
        if n == name:
            break
        name = n
    return name


class Symbolizer:
    def __init__(self, syms):
        self.addrs = [s[0] for s in syms]
        self.syms = syms
        self.cache = {}

    def name(self, addr):
        r = self.cache.get(addr)
        if r is None:
            i = bisect.bisect_right(self.addrs, addr) - 1
            if i < 0 or addr - self.syms[i][0] > max(self.syms[i][1], 1) + 0x1000:
                r = 'k:%x' % addr
            else:
                r = shorten(self.syms[i][2])
            self.cache[addr] = r
        return r


def syscall_names(abi):
    names = {}
    if not abi or not os.path.exists(abi):
        return names
    text = open(abi, encoding='utf8').read()
    m = re.search(r'pub mod nr \{(.*?)\n\}', text, re.S)
    for name, num in re.findall(r'pub const (\w+): u64 = (\d+);', m.group(1) if m else ''):
        names[int(num)] = name
    return names


def process_labels(lines):
    """pid -> label from the kernel's spawn lines."""
    labels = {}
    for ln in lines:
        m = re.search(r'process (\d+) \(([^)]*)\): (/\S+) spawned by (\d+), entry \S+, args \[(.*?)\](?: and (\d+) more)?', ln)
        if not m:
            continue
        pid, name, prog, parent, args = int(m.group(1)), m.group(2), m.group(3), int(m.group(4)), m.group(5)
        a = re.findall(r'"([^"]*)"', args)
        label = '%s %d' % (os.path.basename(prog), pid)
        if prog.endswith('winrun') and len(a) > 1:
            exe = os.path.basename(a[1].replace('\\', '/'))
            if '-contentproc' in a:
                label = 'zen content %d' % pid
            elif exe.lower().startswith('zen'):
                label = 'zen main %d' % pid
            else:
                label = 'winrun %s %d' % (exe, pid)
        labels[pid] = label
    return labels


def module_maps(lines):
    """pid -> sorted [(base, size, name)]"""
    maps = collections.defaultdict(list)
    for ln in lines:
        m = re.search(r'\[winrun:(\d+)\] winrun: module (\S+) base=0x([0-9a-f]+) size=0x([0-9a-f]+)', ln)
        if m:
            maps[int(m.group(1))].append((int(m.group(3), 16), int(m.group(4), 16), m.group(2)))
    for v in maps.values():
        v.sort()
    return maps


def user_frame(maps, pid, rip):
    mods = maps.get(pid, [])
    i = bisect.bisect_right([m[0] for m in mods], rip) - 1
    if i >= 0 and rip < mods[i][0] + mods[i][1]:
        base, size, name = mods[i]
        return name, rip - base
    if 0x400000 <= rip < 0x800000:
        return 'winrun', rip - 0x400000
    return None, rip


class Node:
    __slots__ = ('name', 'value', 'kids', 'kind')

    def __init__(self, name, kind='k'):
        self.name, self.value, self.kids, self.kind = name, 0, {}, kind


def add(root, stack, weight, kind):
    n = root
    n.value += weight
    for s in stack:
        c = n.kids.get(s)
        if c is None:
            c = n.kids[s] = Node(s, kind)
        c.value += weight
        n = c


def color(name, kind):
    h = 0
    for ch in name:
        h = (h * 131 + ord(ch)) & 0xFFFFFF
    v = h % 64
    if kind == 'u':
        return 'rgb(%d,%d,%d)' % (220 + v // 4, 90 + v, 40)
    if kind == 'b':
        return 'rgb(%d,%d,%d)' % (70 + v, 130 + v // 2, 215)
    if kind == 'r':
        return 'rgb(%d,%d,%d)' % (150 + v, 90, 190)
    return 'rgb(%d,%d,%d)' % (230 + v // 4, 130 + v, 60 + v // 2)


JS = r'''
var W=%(W)d,H=16,svg=document.currentScript.ownerSVGElement;
function all(){return svg.querySelectorAll('g.f')}
function show(x,w){all().forEach(function(g){var gx=+g.dataset.x,gw=+g.dataset.w;
 if(gx+gw<=x+1e-6||gx>=x+w-1e-6){g.style.display='none';return}
 g.style.display='';var s=W/w,nx=(gx-x)*s,nw=gw*s;
 var r=g.firstElementChild.nextElementSibling,t=g.lastElementChild;
 r.setAttribute('x',nx);r.setAttribute('width',Math.max(nw,0.1));
 t.setAttribute('x',nx+3);t.textContent=nw>40?g.dataset.n.substring(0,Math.floor(nw/6.5)):''})}
svg.addEventListener('click',function(e){var g=e.target.closest('g.f');
 if(!g||g.dataset.d=='0'){show(0,W);return}show(+g.dataset.x,+g.dataset.w)});
function search(q){all().forEach(function(g){var r=g.firstElementChild.nextElementSibling;
 r.style.stroke=(q&&g.dataset.n.toLowerCase().indexOf(q.toLowerCase())>=0)?'#000':'none'})}
'''


def render_svg(root, title, unit, width=1400):
    total = root.value or 1
    maxd = 0

    def depth(n, d):
        nonlocal maxd
        maxd = max(maxd, d)
        for k in n.kids.values():
            depth(k, d + 1)
    depth(root, 0)
    height = (maxd + 1) * 16 + 64
    out = []
    out.append('<svg xmlns="http://www.w3.org/2000/svg" width="%d" height="%d" font-family="monospace" font-size="11">' % (width, height))
    out.append('<rect width="100%%" height="100%%" fill="#f8f8f8"/><text x="8" y="20" font-size="14">%s</text>' % html.escape(title))
    out.append('<text x="8" y="38" fill="#555">total %d %s - click to zoom, click root to reset</text>' % (total, unit))

    def walk(n, x, d):
        w = n.value / total * width
        if w >= 0.1:
            y = height - 8 - (d + 1) * 16
            nm = html.escape(n.name)
            pct = n.value * 100.0 / total
            out.append('<g class="f" data-x="%.3f" data-w="%.3f" data-d="%d" data-n="%s"><title>%s (%d %s, %.2f%%)</title>'
                       '<rect x="%.3f" y="%d" width="%.3f" height="15" fill="%s" rx="1"/>'
                       '<text x="%.3f" y="%d">%s</text></g>' % (
                           x, w, d, nm, nm, n.value, unit, pct, x, y, w, color(n.name, n.kind) if d else '#bbb', x + 3, y + 11,
                           nm[:int(w / 6.5)] if w > 40 else ''))
            cx = x
            for k in sorted(n.kids.values(), key=lambda k: k.name):
                walk(k, cx, d + 1)
                cx += k.value / total * width
    walk(root, 0.0, 0)
    out.append('<script type="text/ecmascript"><![CDATA[%s]]></script></svg>' % (JS % {'W': width}))
    return '\n'.join(out)


def write_folded(path, flat):
    with open(path, 'w', encoding='utf8') as f:
        for stack, w in sorted(flat.items(), key=lambda kv: -kv[1]):
            f.write('%s %d\n' % (';'.join(stack), w))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('log')
    ap.add_argument('--kernel', default='build/esp/EFI/OSYX/KERNEL.ELF')
    ap.add_argument('--winrun', default='target/user/x86_64-unknown-none/release/winrun')
    ap.add_argument('--abi', default='shared/abi/src/lib.rs')
    ap.add_argument('--out', default='build/flame')
    ap.add_argument('--title', default='OSyx')
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    lines = open(a.log, encoding='utf8', errors='replace').read().splitlines()
    sym = Symbolizer(read_symbols(a.kernel, KERNEL_BASE))
    usyms = {}

    def user_symbols(label):
        # Static user programs (winrun, prism, desktop...) by the program the process runs.
        prog = label.split()[0] if label else ''
        prog = 'winrun' if prog in ('zen', 'winrun') else prog
        if prog not in usyms:
            path = os.path.join(os.path.dirname(a.winrun), prog)
            usyms[prog] = Symbolizer(read_symbols(path)) if os.path.exists(path) else None
        return usyms[prog]
    sysn = syscall_names(a.abi)
    labels = process_labels(lines)
    maps = module_maps(lines)
    begin = next((l for l in lines if 'prof: begin' in l), None)
    if not begin:
        sys.exit('no "prof: begin" line: boot with prof=<seconds> and wait for the dump')
    meta = dict(re.findall(r'(\w+)=(\d+)', begin))
    flat = {k: collections.defaultdict(int) for k in 'cbr'}
    self_k = collections.Counter()
    self_u = collections.Counter()
    xul_rva = collections.Counter()
    per_proc = collections.Counter()
    via_builtin = collections.Counter()
    idle_ms = 0
    for ln in lines:
        m = re.search(r'prof: ([UKBR]) (\d+) (\d+) (\d+) (\d+)((?: [0-9a-f]+)*)$', ln)
        if not m:
            continue
        kind, pid, tid, sysnr, count = m.group(1), int(m.group(2)), int(m.group(3)), int(m.group(4)), int(m.group(5))
        frames = [int(x, 16) for x in m.group(6).split()]
        proc = labels.get(pid, 'kernel' if pid == 0 else 'pid %d' % pid)
        thread = 'tid %d' % tid
        if kind == 'U':
            mod, off = user_frame(maps, pid, frames[0])
            usym = user_symbols(proc) if mod == 'winrun' else None
            callers = [usym.name(x) for x in frames[1:]] if usym else []
            if mod == 'winrun' and usym:
                top = usym.name(frames[0])
            elif mod is None:
                top = 'anon/JIT %x' % (frames[0] >> 12 << 12)
            else:
                top = '%s+%#x' % (mod, off >> 8 << 8)
                if mod.lower() == 'xul.dll':
                    xul_rva[off] += count
            self_u[(mod or 'anon/JIT') if mod != 'winrun' else 'winrun!' + top] += count
            per_proc[proc] += count
            flat['c'][('all', 'user', proc, thread, mod or 'anon/JIT') + tuple(reversed(callers)) + (top,)] += count
            if callers and top.startswith(('compiler_builtins', '__', 'memcpy', 'memset')):
                via_builtin[(proc, thread, callers[0], top)] += count
            continue
        names = [sym.name(f) for f in frames]
        if kind == 'K':
            if any(n.endswith(('sched::secondary_idle', 'sched::idle_without_tick', 'sched::idle_loop')) or n in ('secondary_idle', 'idle_loop') for n in names):
                idle_ms += count
                flat['c'][('all', 'idle')] += count
                continue
            sc = sysn.get(sysnr, str(sysnr)) if sysnr else '-'
            stack = ('all', 'kernel', proc, thread, 'syscall ' + sc) + tuple(reversed(names))
            self_k[names[0] if names else '?'] += count
            per_proc[proc] += count
            flat['c'][stack] += count
        else:
            sc = sysn.get(sysnr, str(sysnr)) if sysnr else '-'
            stack = ('all', 'blocked' if kind == 'B' else 'runnable', proc, thread, 'syscall ' + sc) + tuple(reversed(names))
            flat['b' if kind == 'B' else 'r'][stack] += count
    ticks, idle = int(meta.get('ticks', 0)), int(meta.get('idle_ticks', 0))
    titles = {'c': ('cpu', 'on-CPU (user + kernel), 1 ms per sample', 'ms', 'u'),
              'b': ('offcpu', 'off-CPU: threads blocked in the kernel', 'thread-ms', 'b'),
              'r': ('runq', 'runnable, waiting for a CPU', 'thread-ms', 'r')}
    pages = []
    for key, (fname, title, unit, kind) in titles.items():
        root = Node('all', kind)
        for stack, w in flat[key].items():
            st = stack[1:] if stack[0] == 'all' else stack
            add(root, st, w, kind)
        svg = render_svg(root, '%s - %s' % (a.title, title), unit)
        open(os.path.join(a.out, fname + '.svg'), 'w', encoding='utf8').write(svg)
        write_folded(os.path.join(a.out, fname + '.folded'), flat[key])
        pages.append((title, svg))
    with open(os.path.join(a.out, 'index.html'), 'w', encoding='utf8') as f:
        f.write('<!doctype html><meta charset="utf-8"><title>%s flame graphs</title><body style="margin:8px;background:#f8f8f8">' % html.escape(a.title))
        f.write('<input placeholder="search" oninput="document.querySelectorAll(\'svg\').forEach(function(s){s.querySelectorAll(\'g.f\').forEach(function(g){var r=g.firstElementChild.nextElementSibling;r.style.stroke=(this.value&&g.dataset.n.toLowerCase().indexOf(this.value.toLowerCase())>=0)?\'#000\':\'none\'}.bind(this))}.bind(this))">')
        for title, svg in pages:
            f.write('<h3>%s</h3><div style="overflow:auto">%s</div>' % (html.escape(title), svg))
    cpu_total = sum(flat['c'].values())
    with open(os.path.join(a.out, 'report.md'), 'w', encoding='utf8') as f:
        f.write('# %s\n\nlog `%s`\n\n' % (a.title, a.log))
        f.write('- CPUs %s, window ticks %d, idle ticks (CPU 0) %d, samples %s, dropped %s, distinct stacks %s\n' % (meta.get('cpus'), ticks, idle, meta.get('samples'), meta.get('dropped'), meta.get('stacks')))
        f.write('- on-CPU samples kept: %d ms (user %d, kernel %d, idle %d)\n\n' % (cpu_total, sum(self_u.values()), sum(self_k.values()), idle_ms))
        f.write('## on-CPU by process\n\n| process | ms |\n|---|---|\n')
        for p, v in per_proc.most_common(12):
            f.write('| %s | %d |\n' % (p, v))
        f.write('\n## hottest user modules / winrun functions (self)\n\n| module | ms |\n|---|---|\n')
        for p, v in self_u.most_common(12):
            f.write('| %s | %d |\n' % (p, v))
        f.write('\n## soft-float / memcpy leaf: nearest winrun return address\n\n| process | thread | caller candidate | leaf | ms |\n|---|---|---|---|---|\n')
        for (p, t, c, l), v in via_builtin.most_common(15):
            f.write('| %s | %s | `%s` | `%s` | %d |\n' % (p, t, c[:90], l[:50], v))
        f.write('\n## hottest kernel functions (self)\n\n| function | ms |\n|---|---|\n')
        for p, v in self_k.most_common(25):
            f.write('| `%s` | %d |\n' % (p, v))
        f.write('\n## hottest xul.dll addresses (RVA)\n\n| RVA | ms |\n|---|---|\n')
        for p, v in xul_rva.most_common(25):
            f.write('| %#x | %d |\n' % (p, v))
        for key, (fname, title, unit, kind) in titles.items():
            if key == 'c':
                continue
            agg = collections.Counter()
            for stack, w in flat[key].items():
                agg[(stack[2], stack[4], stack[5] if len(stack) > 5 else '')] += w
            f.write('\n## %s: heaviest (process, syscall, innermost kernel function)\n\n| process | syscall | function | thread-ms |\n|---|---|---|---|\n' % title)
            for (p, s, fn), v in agg.most_common(25):
                f.write('| %s | %s | `%s` | %d |\n' % (p, s, fn, v))
    print('wrote', a.out)


if __name__ == '__main__':
    main()
