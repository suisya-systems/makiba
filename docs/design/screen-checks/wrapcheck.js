// Injected by wrapcheck.sh into a copy of the drawing only (the drawing itself has no script). For
// every block of text on the rendered page, finds its lines by the position of each word, and
// reports every block of two lines or more that has a line holding a single word; then every row
// cut by the bottom of its list or of the right column, or drawn under a list's strip.
(function () {
  function blockOf(node) {
    let e = node.parentElement;
    while (e && getComputedStyle(e).display.startsWith('inline')) e = e.parentElement;
    return e;
  }
  function visible(e) {
    for (let x = e; x; x = x.parentElement) {
      const cs = getComputedStyle(x);
      if (cs.display === 'none' || cs.visibility === 'hidden') return false;
    }
    return true;
  }
  const blocks = new Map();
  const tw = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  while (tw.nextNode()) {
    const t = tw.currentNode;
    if (!t.textContent.trim() || !t.parentElement || t.parentElement.closest('style,script,svg,#wrapcheck')) continue;
    const b = blockOf(t);
    if (!b || !visible(b)) continue;
    if (!blocks.has(b)) blocks.set(b, []);
    const re = /\S+/g;
    let m;
    while ((m = re.exec(t.textContent))) {
      const r = document.createRange();
      r.setStart(t, m.index);
      r.setEnd(t, m.index + m[0].length);
      const rs = Array.from(r.getClientRects()).filter(x => x.width > 0);
      if (!rs.length) continue;
      // a word cut across two lines counts on each line it touches
      for (const rc of rs) blocks.get(b).push({ w: m[0], mid: (rc.top + rc.bottom) / 2, h: rc.height });
    }
  }
  const found = [];
  let checked = 0;
  for (const [b, words] of blocks) {
    if (!words.length) continue;
    checked++;
    words.sort((a, c) => a.mid - c.mid);
    const lines = [];
    for (const w of words) {
      const last = lines[lines.length - 1];
      if (last && Math.abs(w.mid - last.mid) < Math.max(6, 0.5 * Math.min(w.h, last.h))) last.words.push(w.w);
      else lines.push({ mid: w.mid, h: w.h, words: [w.w] });
    }
    if (lines.length < 2) continue;
    lines.forEach((l, i) => {
      if (l.words.length === 1) {
        const where = b.closest('[data-region],.panel,.cap,.doc');
        const region = where ? (where.getAttribute('data-region') || where.className) : '';
        const screen = b.closest('.screen,.panels');
        const pic = screen ? (screen.className) : 'page';
        found.push(`${pic} | ${region} | line ${i + 1} of ${lines.length}: "${l.words[0]}" | block: ${lines.map(x => x.words.join(' ')).join(' / ').slice(0, 160)}`);
      }
    });
  }
  // a row cut by the bottom of its place: in a list above its strip, or at the foot of the right column
  const cut = [];
  document.querySelectorAll('.list').forEach(list => {
    const strip = list.querySelector('.below');
    const lb = list.getBoundingClientRect();
    const limit = strip ? strip.getBoundingClientRect().top : lb.bottom;
    list.querySelectorAll('.row,.gh,.repo .hd,.repo .sum,.fold').forEach(e => {
      const r = e.getBoundingClientRect();
      if (r.height && r.top < limit - 1 && r.bottom > limit + 1) cut.push('cut above the strip: ' + e.textContent.trim().replace(/\s+/g, ' ').slice(0, 90));
      if (r.height && r.top >= limit - 1 && r.top < lb.bottom) cut.push('drawn under the strip: ' + e.textContent.trim().replace(/\s+/g, ' ').slice(0, 90));
    });
  });
  document.querySelectorAll('.watch').forEach(w => {
    const wb = w.getBoundingClientRect().bottom;
    w.querySelectorAll('.fr,.c2,.fold,.sh').forEach(e => {
      const r = e.getBoundingClientRect();
      if (r.height && r.bottom > wb + 1 && r.top < wb) cut.push('cut at the foot of the right column: ' + e.textContent.trim().replace(/\s+/g, ' ').slice(0, 90));
    });
  });
  const pre = document.createElement('pre');
  pre.id = 'wrapcheck';
  pre.textContent = `checked ${checked} blocks\n` + (found.length ? found.join('\n') : 'no line holds a single word') + '\n' + (cut.length ? cut.join('\n') : 'no row is cut');
  document.body.appendChild(pre);
})();
