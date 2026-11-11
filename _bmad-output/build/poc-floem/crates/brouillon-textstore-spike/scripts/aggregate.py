import re, sys, collections
rows = collections.OrderedDict()
early = {}
peak = {}
for line in open(sys.argv[1], encoding='utf8', errors='replace'):
    line = line.strip()
    if line.startswith('R|'):
        _, store, f, stage, ms, hl, hp, ws, pv, note = (line.split('|', 9) + [''])[:10]
        if stage.startswith('early'):
            early[(f, store)] = (float(ms), note)
            continue
        rows.setdefault(f, {}).setdefault(store, {})[stage.split(' ')[0]] = (float(ms), float(hl), float(hp), float(ws), float(pv), note)
    elif line.startswith('PEAK|'):
        _, store, f, note = line.split('|', 3)
        peak[(f, store)] = note

def kv(note, k):
    m = re.search(k + r'=([0-9.]+)', note)
    return float(m.group(1)) if m else None

def key(f):
    m = re.match(r's(\d+)_(\w+)_(\w+)\.txt', f)
    return (int(m.group(1)), m.group(2), m.group(3))

out = []
for f in sorted(rows, key=key):
    out.append(f'\n#### {f}\n')
    out.append('| metric | rope (crop) | lazy mmap | lazy chunked |')
    out.append('|---|---|---|---|')
    S = ['rope', 'mmap', 'chunked']
    def g(store, stage, idx):
        r = rows[f].get(store, {}).get(stage)
        return r[idx] if r else None
    def line(name, fn):
        vals = []
        for s in S:
            try:
                v = fn(s)
            except Exception:
                v = None
            vals.append('-' if v is None else (v if isinstance(v, str) else f'{v:.1f}'))
        out.append(f'| {name} | ' + ' | '.join(vals) + ' |')
    line('private MB, after open', lambda s: g(s, '1b', 4))
    line('private MB, index complete', lambda s: g(s, '2', 4))
    line('private MB, after 300 jumps', lambda s: g(s, '3', 4))
    line('private MB, after 10k edits', lambda s: g(s, '4', 4))
    line('private MB, after Find', lambda s: g(s, '5', 4))
    line('private MB, after Save', lambda s: g(s, '6', 4))
    line('private MB, after undo-all', lambda s: g(s, '7', 4))
    line('working set MB, after open', lambda s: g(s, '1b', 3))
    line('working set MB, after Find', lambda s: g(s, '5', 3))
    line('working set MB, after undo-all (peak)', lambda s: kv(peak.get((f, s), ''), 'peak_ws'))
    line('peak commit MB (whole run)', lambda s: kv(peak.get((f, s), ''), 'peak_commit'))
    line('heap live MB, after 10k edits', lambda s: g(s, '4', 1))
    line('heap peak MB during Save', lambda s: g(s, '6', 2))
    line('open ms', lambda s: g(s, '1', 0))
    line('first viewport ms (line N, 60 rows)', lambda s: g(s, '1b', 0))
    line('index complete / line_count ms', lambda s: g(s, '2', 0))
    line('jump p50 / p99 ms', lambda s: f"{kv(rows[f][s]['3'][5],'p50_ms'):.3f} / {kv(rows[f][s]['3'][5],'p99_ms'):.3f}")
    line('edit p50 / p99 / max ms', lambda s: f"{kv(rows[f][s]['4'][5],'p50_ms'):.4f} / {kv(rows[f][s]['4'][5],'p99_ms'):.4f} / {kv(rows[f][s]['4'][5],'max_ms'):.2f}")
    line('jump after edits p50 / p99 ms', lambda s: f"{kv(rows[f][s]['4b'][5],'p50_ms'):.3f} / {kv(rows[f][s]['4b'][5],'p99_ms'):.3f}")
    line('Find whole file ms', lambda s: g(s, '5', 0))
    line('Save ms', lambda s: g(s, '6', 0))
    line('undo op p50 / p99 ms', lambda s: f"{kv(rows[f][s]['7'][5],'p50_ms'):.4f} / {kv(rows[f][s]['7'][5],'p99_ms'):.4f}")
    line('first edit right after open ms', lambda s: early[(f, s)][0])
    line('open ms (early run)', lambda s: kv(early[(f, s)][1], 'open_ms'))
print('\n'.join(out))
