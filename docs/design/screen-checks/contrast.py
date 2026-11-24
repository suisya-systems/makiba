"""Computes the WCAG 2 contrast ratio of every colour pair the screen draft uses, from the token
values of its .theme-light and .theme-dark rules: text pairs need 4.5:1 (AA), marks and the focus
ring 3:1. Run from the repository root: python3 docs/design/screen-checks/contrast.py [page.html|file.css]
Exit 1 when a pair is below its minimum."""
import re, sys
from pathlib import Path


def lum(h):
    h = h.lstrip('#')
    r, g, b = [int(h[i:i + 2], 16) / 255 for i in (0, 2, 4)]
    f = lambda c: c / 12.92 if c <= 0.03928 else ((c + 0.055) / 1.055) ** 2.4
    return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b)


def cr(a, b):
    la, lb = sorted([lum(a), lum(b)], reverse=True)
    return (la + 0.05) / (lb + 0.05)


def tokens(css, selector):
    """The custom properties of the first rule whose selector list contains `selector`."""
    for m in re.finditer(r'([^{}]+)\{([^{}]*)\}', css):
        if selector in [s.strip() for s in m.group(1).split(',')]:
            return dict(re.findall(r'--([\w-]+):\s*(#[0-9a-fA-F]{6})', m.group(2)))
    raise SystemExit(f'no rule {selector}')


# The drawing's own CSS: the <style> of the page (or a .css file given instead).
src = Path(sys.argv[1] if len(sys.argv) > 1 else 'docs/design/screen-draft.html').read_text(encoding='utf-8')
if src.lstrip().lower().startswith('<!doctype') or '<style' in src:
    src = '\n'.join(re.findall(r'<style[^>]*>(.*?)</style>', src, flags=re.S))
css = re.sub(r'/\*.*?\*/', '', src, flags=re.S)
low = 0
for name, sel in [('light', '.theme-light'), ('dark', '.theme-dark')]:
    t = tokens(css, sel)
    rows = []
    for fg in ['text', 'text-2', 'text-3', 'accent', 'ok', 'wait', 'fail', 'info']:
        for bg in ['bg', 'surface', 'surface-2']:
            rows.append((f'{fg} on {bg}', cr(t[fg], t[bg]), 4.5))
    for s in ['ok', 'wait', 'fail', 'info']:
        rows.append((f'{s} on {s}-bg (badge word)', cr(t[s], t[s + '-bg']), 4.5))
        rows.append((f'{s}-mark on surface (mark, non-text)', cr(t[s + '-mark'], t['surface']), 3))
        rows.append((f'{s}-mark on {s}-bg (mark in badge)', cr(t[s + '-mark'], t[s + '-bg']), 3))
        rows.append((f'on-solid on {s}-solid (filled button / badge)', cr(t['on-solid'], t[s + '-solid']), 4.5))
        rows.append((f'text on {s}-row (tinted row)', cr(t['text'], t[s + '-row']), 4.5))
        rows.append((f'text-3 on {s}-row (tinted row, meta)', cr(t['text-3'], t[s + '-row']), 4.5))
    rows.append(('idle-mark on surface (mark, non-text)', cr(t['idle-mark'], t['surface']), 3))
    rows.append(('accent on surface (focus ring, non-text)', cr(t['accent'], t['surface']), 3))
    rows.append(('on-primary on primary (primary button)', cr(t['on-primary'], t['primary']), 4.5))
    rows.append(('term-text on term', cr(t['term-text'], t['term']), 4.5))
    rows.append(('term-dim on term', cr(t['term-dim'], t['term']), 4.5))
    rows.append(('gap-text on gap-bg (missing field)', cr(t['gap-text'], t['gap-bg']), 4.5))
    for s_ in ['ok', 'wait', 'fail', 'info']:
        rows.append((f'{s_} on {s_}-row (state word in a tinted row)', cr(t[s_], t[s_ + '-row']), 4.5))
        rows.append((f'text-2 on {s_}-row', cr(t['text-2'], t[s_ + '-row']), 4.5))
    rows.append(('wait on fail-row (a tab that is red with a needs-you word)', cr(t['wait'], t['fail-row']), 4.5))
    rows.append(('term-dim on term-2 (terminal bar)', cr(t['term-dim'], t['term-2']), 4.5))
    rows.append(('term-text on term-2 (terminal bar)', cr(t['term-text'], t['term-2']), 4.5))
    rows.append(('text-2 on surface-2 (tags, if boxes)', cr(t['text-2'], t['surface-2']), 4.5))
    rows.append(('page-text on page (around the drawings)', cr(t['page-text'], t['page']), 4.5))
    print(f'== {name}')
    for n, v, m in rows:
        ok_ = v >= m
        low += not ok_
        print(f'{"ok " if ok_ else "LOW"} {v:5.2f} (min {m}) {n}')
print('all pass' if not low else f'{low} below')
sys.exit(bool(low))
