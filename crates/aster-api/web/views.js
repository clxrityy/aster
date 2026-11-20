// View helpers. Every piece of text goes through textContent; nothing is
// parsed as HTML, so learned or ingested text can never inject markup.

export const LEVELS = ['energy', 'focus', 'cognitive_load', 'curiosity', 'stimulation', 'stress', 'confidence'];
const SVG = 'http://www.w3.org/2000/svg';

/** h('div.card#id', {onclick, title, ...}, children...) */
export function h(spec, attrs, ...children) {
  if (attrs === null || typeof attrs !== 'object' || attrs instanceof Node || Array.isArray(attrs)) {
    children.unshift(attrs);
    attrs = {};
  }
  const [tag, ...classes] = spec.split('.');
  const [name, id] = tag.split('#');
  const el = document.createElement(name || 'div');
  if (id) el.id = id;
  if (classes.length) el.className = classes.join(' ');
  for (const [key, value] of Object.entries(attrs)) {
    if (value === undefined || value === null || value === false) continue;
    if (key.startsWith('on')) el.addEventListener(key.slice(2), value);
    else if (key === 'class') el.className += ` ${value}`;
    // CSSOM, not the style attribute: the CSP forbids inline style attributes.
    else if (key === 'style') el.style.cssText = value;
    else if (key === 'value') el.value = value;
    else if (key === 'checked') el.checked = value;
    else el.setAttribute(key, value === true ? '' : value);
  }
  append(el, children);
  return el;
}

/** Replace an element's children, skipping null/false like h() does. */
export function fill(el, ...children) {
  el.replaceChildren();
  append(el, children);
  return el;
}

export function append(el, ...children) {
  for (const child of children.flat(Infinity)) {
    if (child === undefined || child === null || child === false) continue;
    el.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
}

function s(tag, attrs = {}, ...children) {
  const el = document.createElementNS(SVG, tag);
  for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v);
  for (const c of children) el.append(c);
  return el;
}

export const label = (name) => name.replace(/_/g, ' ');
export const pct = (v) => `${Math.round((Number(v) || 0) * 100)}%`;
export const fixed = (v, d = 2) => (Number.isFinite(Number(v)) ? Number(v).toFixed(d) : '—');

export function time(iso) {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? '' : d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

export function when(iso) {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? '' : d.toLocaleString([], { dateStyle: 'medium', timeStyle: 'short' });
}

export function bytes(n) {
  if (n < 1024) return `${n} B`;
  if (n < 1 << 20) return `${(n / 1024).toFixed(1)} KiB`;
  return `${(n / (1 << 20)).toFixed(1)} MiB`;
}

export function sparkline(values) {
  const width = 44;
  const height = 18;
  const pts = values.slice(-24);
  if (pts.length < 2) return s('svg', { class: 'spark', viewBox: `0 0 ${width} ${height}` });
  const min = Math.min(...pts);
  const max = Math.max(...pts);
  const span = max - min || 1;
  const coords = pts.map((v, i) => `${(i / (pts.length - 1)) * width},${height - 2 - ((v - min) / span) * (height - 4)}`);
  return s('svg', { class: 'spark', viewBox: `0 0 ${width} ${height}`, 'aria-hidden': 'true' },
    s('polyline', { points: coords.join(' ') }));
}

/** A small multi-series line chart. series: [{ name, color, points: [[x, y]] }] */
export function lineChart(series, { yMin, yMax, xLabel = (x) => x, height = 160 } = {}) {
  const width = 600;
  const pad = { l: 34, r: 8, t: 8, b: 18 };
  const all = series.flatMap((x) => x.points);
  const svg = s('svg', { class: 'chart', viewBox: `0 0 ${width} ${height}`, preserveAspectRatio: 'none', role: 'img' });
  if (all.length < 2) {
    svg.append(s('text', { x: width / 2, y: height / 2, 'text-anchor': 'middle', class: 'axis' }, 'not enough data yet'));
    return svg;
  }
  const xs = all.map((p) => p[0]);
  const ys = all.map((p) => p[1]);
  const x0 = Math.min(...xs);
  const x1 = Math.max(...xs);
  const y0 = yMin ?? Math.min(...ys);
  const y1 = yMax ?? Math.max(...ys);
  const X = (x) => pad.l + ((x - x0) / (x1 - x0 || 1)) * (width - pad.l - pad.r);
  const Y = (y) => height - pad.b - ((y - y0) / (y1 - y0 || 1)) * (height - pad.t - pad.b);
  for (let i = 0; i <= 4; i += 1) {
    const v = y0 + ((y1 - y0) * i) / 4;
    svg.append(s('line', { x1: pad.l, x2: width - pad.r, y1: Y(v), y2: Y(v), class: 'grid' }));
    svg.append(s('text', { x: pad.l - 4, y: Y(v) + 3, 'text-anchor': 'end', class: 'axis' }, fixed(v, y1 - y0 < 5 ? 2 : 0)));
  }
  svg.append(s('text', { x: pad.l, y: height - 4, class: 'axis' }, String(xLabel(x0))));
  svg.append(s('text', { x: width - pad.r, y: height - 4, 'text-anchor': 'end', class: 'axis' }, String(xLabel(x1))));
  for (const line of series) {
    if (line.points.length < 2) continue;
    svg.append(s('polyline', {
      points: line.points.map(([x, y]) => `${X(x)},${Y(y)}`).join(' '),
      stroke: line.color,
      'vector-effect': 'non-scaling-stroke',
    }, s('title', {}, line.name)));
  }
  return svg;
}

export const SERIES_COLORS = ['#7c6cf2', '#2f9e74', '#d08a2c', '#3c8dde', '#c95fb5', '#d4543f', '#8a8f2e'];

function hot(name, value) {
  return ((name === 'stress' || name === 'cognitive_load') && value > 60) || (name === 'energy' && value < 25);
}

function levelRow(name, levels, history) {
  const value = Number(levels?.[name] ?? 0);
  const past = history[name] || [];
  const prev = past.length > 1 ? past[past.length - 2] : value;
  const trend = value - prev > 0.05 ? '↑' : prev - value > 0.05 ? '↓' : '';
  return {
    value, past, hot: hot(name, value), width: `${Math.max(0, Math.min(100, value))}%`,
    num: `${Math.round(value)}${trend}`, title: `${label(name)}: ${fixed(value, 1)}`,
  };
}

export function levelBars(levels, history) {
  return h('div.levels', LEVELS.map((name) => {
    const row = levelRow(name, levels, history);
    return h('div.level', { title: row.title, 'data-level': name },
      h('span', label(name)),
      h(`div.bar${row.hot ? '.hot' : ''}`, h('i', { style: `width:${row.width}` })),
      h('span.num', row.num),
      sparkline(row.past));
  }));
}

/** Update bars made by levelBars in place, so widths animate instead of the DOM being rebuilt. */
export function refreshLevelBars(el, levels, history) {
  for (const rowEl of el.querySelectorAll('.level[data-level]')) {
    const row = levelRow(rowEl.dataset.level, levels, history);
    rowEl.title = row.title;
    const bar = rowEl.querySelector('.bar');
    bar.classList.toggle('hot', row.hot);
    bar.firstElementChild.style.width = row.width;
    rowEl.querySelector('.num').textContent = row.num;
    rowEl.querySelector('svg.spark')?.replaceWith(sparkline(row.past));
  }
}

/** The thought-trace inspector with steering controls. */
export function inspector(trace, { signals = [], onPrefer, onRate, onIrrelevant, onReconsider, onClose }) {
  const chosen = trace.chosen;
  const confidence = Number(trace.confidence) || 0;
  const threshold = Number(trace.threshold) || 0;
  const best = Math.max(0.0001, ...trace.candidates.map((c) => Number(c.score) || 0), threshold);
  const meter = h('div.meter', { title: `confidence ${fixed(confidence)} / threshold ${fixed(threshold)}` },
    h('i', { style: `width:${Math.min(100, confidence * 100)}%` }),
    h('b', { style: `left:${Math.min(100, threshold * 100)}%` }));

  const candidates = trace.candidates.map((c, i) => {
    const isChosen = i === chosen;
    const steerable = c.source !== 'question';
    const parts = Object.entries(c.components || {}).map(([k, v]) => `${label(k)} ${fixed(v)}`).join(' · ');
    return h(`div.cand${isChosen ? '.chosen' : ''}`,
      h('div.top',
        h('span.mono', `#${i + 1}`),
        h(`span.src.${c.source}`, c.source),
        h('span.grow', parts),
        h('span.mono', fixed(c.score))),
      h('div.bar', h('i', { style: `width:${(Number(c.score) / best) * 100}%` })),
      h('div.text', c.text),
      steerable && h('div.row',
        !isChosen && h('button.btn.small', { onclick: () => onPrefer(i), title: 'Make this the reply for this prompt' }, 'Use this'),
        isChosen && h('span.chip.accent', 'chosen'),
        h('button.btn.small.ghost.good', { onclick: () => onRate(i, 1), title: 'Good candidate', 'aria-label': `Rate candidate ${i + 1} good` }, '▲'),
        h('button.btn.small.ghost.bad', { onclick: () => onRate(i, -1), title: 'Poor candidate', 'aria-label': `Rate candidate ${i + 1} poor` }, '▼')));
  });

  const recalled = (trace.recalled || []).map((r, i) => h('div.item',
    h('div.row.small', h('span.mono.muted', r.source), h('span.grow'), h('span.mono', fixed(r.score))),
    h('div.small', r.excerpt),
    h('div', h('button.btn.small', { onclick: () => onIrrelevant(i) }, 'Not relevant here'))));

  return h('div.card.inspector.stack',
    h('div.head', h('h3', 'Thought trace'), h('button.btn.small.ghost', { onclick: onClose, 'aria-label': 'Close trace' }, '✕')),
    h('div.row.small',
      h('span.chip.accent', trace.decision),
      h('span.chip', trace.engine),
      h('span.chip', `explore ${fixed(trace.exploration)}`),
      trace.explored && h('span.chip.warn', 'explored')),
    h('div.stack', { style: 'gap:4px' },
      h('div.row.small.muted', h('span.grow', `confidence ${fixed(confidence)}`), h('span', `threshold ${fixed(threshold)}`)),
      meter),
    trace.unknown_terms?.length ? h('div.small', h('span.muted', 'unknown: '), trace.unknown_terms.join(', ')) : null,
    h('h3', `Candidates (${trace.candidates.length})`),
    h('div.list', candidates),
    h('button.btn', { onclick: onReconsider, title: 'Ask Aster for a different reply to the same message' }, 'Reconsider this reply'),
    recalled.length ? [h('h3', 'Recalled'), h('div.list', recalled)] : null,
    trace.notes?.length ? [h('h3', 'Notes'), h('ul.notes', trace.notes.map((n) => h('li', n)))] : null,
    signals.length ? [h('h3', 'Your signals on this reply'),
      h('ul.notes', signals.map((x) => h('li', `${x.kind}${x.candidate_index != null ? ` #${x.candidate_index + 1}` : ''}${x.rating ? (x.rating > 0 ? ' ▲' : ' ▼') : ''} · ${time(x.created_at)}`)))] : null);
}
