// Aster dashboard: state, live events, and pages.
import { ApiError, del, get, patch, post, setCsrf, stream, whenUnauthorized } from './api.js';
import {
  LEVELS, SERIES_COLORS, append, bytes, fill, fixed, h, inspector, label, levelBars, lineChart, time, when,
} from './views.js';

const PAGES = [
  ['talk', 'Talk'],
  ['inbox', 'Inbox'],
  ['mind', 'Mind'],
  ['neural', 'Neural'],
  ['memories', 'Memories'],
  ['teach', 'Teach & ingest'],
  ['config', 'Config'],
  ['snapshots', 'Snapshots'],
  ['logs', 'Logs'],
];
const LOG_CATEGORIES = ['conversations', 'thoughts', 'levels', 'memories', 'requests', 'config', 'runtime', 'training'];
const root = document.getElementById('root');

const state = {
  page: pageFromHash(),
  live: false,
  turns: [],
  turnIds: new Set(),
  traces: new Map(),
  signals: new Map(),
  ratings: new Map(),
  selected: null,
  correcting: null,
  inbox: [],
  levels: {},
  levelHistory: Object.fromEntries(LEVELS.map((n) => [n, []])),
  runtime: {},
  mode: 'chat',
  replyTo: null,
  training: [],
  stopStream: null,
};

// ----- small utilities ------------------------------------------------------

function pageFromHash() {
  const page = location.hash.replace('#', '');
  return PAGES.some(([id]) => id === page) ? page : 'talk';
}

function toast(message, bad = false) {
  let box = document.querySelector('.toasts');
  if (!box) {
    box = h('div.toasts', { role: 'status' });
    document.body.append(box);
  }
  const el = h(`div.toast${bad ? '.bad' : ''}`, message);
  box.append(el);
  setTimeout(() => el.remove(), bad ? 6000 : 3000);
}

async function attempt(fn, done) {
  try {
    const result = await fn();
    if (done) toast(typeof done === 'function' ? done(result) : done);
    return result;
  } catch (error) {
    toast(error instanceof ApiError ? error.message : String(error), true);
    return undefined;
  }
}

function pushLevels(levels) {
  if (!levels || typeof levels !== 'object') return;
  state.levels = levels;
  for (const name of LEVELS) {
    if (typeof levels[name] !== 'number') continue;
    const series = state.levelHistory[name];
    series.push(levels[name]);
    if (series.length > 48) series.shift();
  }
}

function addTurn(turn) {
  if (!turn?.id || state.turnIds.has(turn.id)) return false;
  state.turnIds.add(turn.id);
  state.turns.push(turn);
  state.turns.sort((a, b) => a.created_at.localeCompare(b.created_at));
  return true;
}

function upsertInbox(item) {
  const index = state.inbox.findIndex((x) => x.id === item.id);
  if (item.status !== 'pending') {
    if (index >= 0) state.inbox.splice(index, 1);
  } else if (index >= 0) state.inbox[index] = item;
  else state.inbox.unshift(item);
}

// ----- sign in --------------------------------------------------------------

function showLogin(message = '') {
  if (state.stopStream) state.stopStream();
  state.stopStream = null;
  const input = h('input#token', {
    type: 'password', autocomplete: 'off', spellcheck: 'false', placeholder: '64 hex characters', required: true,
  });
  const error = h('div.error', message);
  const button = h('button.btn.primary', { type: 'submit' }, 'Sign in');
  const form = h('form', {
    onsubmit: async (event) => {
      event.preventDefault();
      button.disabled = true;
      error.textContent = '';
      try {
        const session = await post('/v1/session', { token: input.value.trim() });
        input.value = '';
        setCsrf(session.csrf);
        await start();
      } catch (e) {
        error.textContent = e.status === 429 ? 'Too many attempts. Wait a moment and try again.'
          : e.status === 401 ? 'That token is not right.' : e.message;
        button.disabled = false;
      }
    },
  },
  h('h1', h('img', { src: '/assets/icon.svg', alt: '' }), 'Aster'),
  h('p.muted.small', 'Sign in with your owner token. The browser keeps only a session cookie, never the token.'),
  h('label.field', h('span', 'Owner token'), input),
  error,
  button);
  fill(root, h('div.login', form));
  input.focus();
}

whenUnauthorized(() => showLogin('Your session ended. Sign in again.'));

// ----- data -----------------------------------------------------------------

async function resync() {
  const [turns, inbox, runtime] = await Promise.all([
    get('/v1/turns?limit=100').catch(() => []),
    get('/v1/outbound?status=pending&limit=100').catch(() => []),
    get('/v1/runtime').catch(() => null),
  ]);
  for (const turn of turns) addTurn(turn);
  state.inbox = inbox;
  if (runtime) {
    state.runtime = runtime;
    pushLevels(runtime.levels);
  }
  render();
}

function onEvent(event) {
  switch (event.type) {
    case 'turn':
      if (addTurn(event.turn) && state.page === 'talk') renderTalk();
      break;
    case 'thought':
      state.traces.set(event.turn_id, event.trace);
      if (state.selected === event.turn_id) renderTalk();
      break;
    case 'levels':
      pushLevels(event.state);
      if (state.page === 'talk' || state.page === 'mind') render();
      break;
    case 'outbound':
      upsertInbox(event.item);
      if (event.item.status === 'pending' && !event.item.origin_turn) {
        toast(`Aster ${event.item.kind === 'request' ? 'requests' : 'asks'}: ${event.item.draft.body}`);
      }
      renderChrome();
      if (state.page === 'inbox') render();
      break;
    case 'trained':
      state.training.push({ step: event.total_steps, loss: event.loss });
      if (state.training.length > 600) state.training.shift();
      if (state.page === 'neural') updateNeural(event);
      break;
    case 'runtime':
      state.runtime.paused = event.paused;
      if (event.message.startsWith('resting')) state.runtime.resting = true;
      if (!event.message.startsWith('mind snapshot')) toast(event.message);
      if (event.message.includes('engine switched')) refreshRuntime();
      renderChrome();
      break;
    case 'config_changed':
      toast(`config: ${event.keys.join(', ')}${event.restart_required.length ? ' (restart needed)' : ''}`);
      break;
    case 'config_rejected':
      toast(`aster.toml rejected: ${event.error}`, true);
      break;
    case 'learned':
      // The action that caused it already reported its note.
      break;
    default:
      break;
  }
}

async function refreshRuntime() {
  const runtime = await get('/v1/runtime').catch(() => null);
  if (runtime) {
    state.runtime = runtime;
    renderChrome();
  }
}

async function start() {
  const session = await get('/v1/session').catch(() => null);
  if (!session?.authenticated) {
    showLogin();
    return;
  }
  setCsrf(session.csrf);
  renderShell();
  await resync();
  state.stopStream = stream({
    onEvent,
    onState: (live) => { state.live = live; renderChrome(); },
    onResync: () => resync(),
  });
}

// ----- shell ----------------------------------------------------------------

let chrome;
let nav;
let main;

function renderShell() {
  chrome = h('header.topbar');
  nav = h('nav.side', { 'aria-label': 'Sections' });
  main = h('main');
  fill(root, h('div.shell', chrome, h('div.body', nav, main)));
  render();
}

function renderChrome() {
  if (!chrome) return;
  const r = state.runtime;
  fill(chrome, 
    h('div.brand', h('img', { src: '/assets/icon.svg', alt: '' }), 'Aster'),
    h('span.chip', { title: state.live ? 'Live events connected' : 'Reconnecting…' },
      h(`span.dot${state.live ? '.live' : ''}`), state.live ? 'live' : 'offline'),
    r.engine && h('span.chip.accent', r.engine),
    r.paused && h('span.chip.warn', 'paused'),
    r.resting && h('span.chip.warn', 'resting'),
    h('span.grow'),
    h('button.btn.small', {
      onclick: () => attempt(() => post(`/v1/runtime/${r.paused ? 'resume' : 'pause'}`), r.paused ? 'resumed' : 'paused')
        .then(refreshRuntime),
    }, r.paused ? 'Resume' : 'Pause'),
    h('button.btn.small.ghost', {
      onclick: async () => {
        await post('/v1/session/logout').catch(() => null);
        setCsrf(null);
        showLogin('Signed out.');
      },
    }, 'Sign out'));
  fill(nav, ...PAGES.map(([id, name]) => h(`button${state.page === id ? '.on' : ''}`, {
    onclick: () => { location.hash = id; },
    'aria-current': state.page === id ? 'page' : null,
  }, name, id === 'inbox' && state.inbox.length ? h('span.count', state.inbox.length) : null)));
}

window.addEventListener('hashchange', () => {
  state.page = pageFromHash();
  render();
});

function render() {
  if (!main) return;
  renderChrome();
  const pages = {
    talk: renderTalk,
    inbox: renderInbox,
    mind: renderMind,
    neural: renderNeural,
    memories: renderMemories,
    teach: renderTeach,
    config: renderConfig,
    snapshots: renderSnapshots,
    logs: renderLogs,
  };
  pages[state.page]();
}

// ----- talk -----------------------------------------------------------------

let composer;
let transcriptEl;

function buildComposer() {
  const text = h('textarea', {
    rows: 2,
    placeholder: 'Talk to Aster…  (Enter sends, Shift+Enter for a new line)',
    'aria-label': 'Message',
    onkeydown: (event) => {
      if (event.key === 'Enter' && !event.shiftKey) {
        event.preventDefault();
        send();
      }
    },
  });
  const send = async () => {
    const value = text.value.trim();
    if (!value) return;
    text.value = '';
    const body = { text: value, mode: state.mode };
    if (state.replyTo) body.reply_to = state.replyTo;
    state.replyTo = null;
    const result = await attempt(() => post('/v1/chat', body));
    if (!result) {
      text.value = value;
      return;
    }
    addTurn(result.owner);
    addTurn(result.aster);
    state.traces.set(result.aster.id, result.trace);
    if (result.outbound) upsertInbox(result.outbound);
    pushLevels(result.levels);
    if (state.selected) state.selected = result.aster.id;
    renderTalk();
  };
  const holder = h('div.composer');
  composer = { el: holder, text, send };
  return composer;
}

function aster(turn) {
  return turn.role === 'aster';
}

function messageView(turn) {
  const rating = state.ratings.get(turn.id);
  const parent = turn.reply_to && state.turns.find((t) => t.id === turn.reply_to);
  const threaded = turn.role === 'owner' && parent && parent.role === 'aster';
  const reconsidered = aster(turn) && state.turns.some((t) => aster(t) && t.reply_to === turn.reply_to && t.id !== turn.id
    && t.created_at < turn.created_at);
  const correcting = state.correcting === turn.id;
  const correction = correcting && h('form.row', {
    onsubmit: async (event) => {
      event.preventDefault();
      const value = event.target.elements.correction.value.trim();
      if (!value) return;
      if (await attempt(() => post('/v1/feedback', { turn_id: turn.id, rating: -1, correction: value }), 'correction learned') !== undefined) {
        state.ratings.set(turn.id, -1);
        state.correcting = null;
        renderTalk();
      }
    },
  }, h('input.grow', { type: 'text', name: 'correction', placeholder: 'What should Aster have said?', 'aria-label': 'Correction' }),
  h('button.btn.small.primary', { type: 'submit' }, 'Teach'),
  h('button.btn.small.ghost', { type: 'button', onclick: () => { state.correcting = null; renderTalk(); } }, 'Cancel'));

  const rate = async (value) => {
    if (await attempt(() => post('/v1/feedback', { turn_id: turn.id, rating: value }), value > 0 ? 'thanks' : 'noted') !== undefined) {
      state.ratings.set(turn.id, value);
      renderTalk();
    }
  };

  return h(`div.msg.${turn.role}${state.selected === turn.id ? '.selected' : ''}`, { 'data-id': turn.id },
    threaded && h('div.reply-to', `↪ ${parent.text.slice(0, 120)}`),
    reconsidered && parent && h('div.reply-to', `↺ another reply to “${parent.text.slice(0, 120)}”`),
    h('div.meta',
      h('span', aster(turn) ? 'Aster' : 'You'),
      h('span', time(turn.created_at)),
      turn.mode === 'train' && h('span.chip', 'train'),
      reconsidered && h('span.chip', 'reconsidered'),
      rating && h(`span.${rating > 0 ? 'rated-up' : 'rated-down'}`, rating > 0 ? '▲' : '▼')),
    h('div.text', turn.text),
    aster(turn) && h('div.actions',
      h('button.btn.small.ghost.good', { onclick: () => rate(1), title: 'Good reply', 'aria-label': 'Good reply' }, '▲'),
      h('button.btn.small.ghost.bad', { onclick: () => rate(-1), title: 'Poor reply', 'aria-label': 'Poor reply' }, '▼'),
      h('button.btn.small.ghost', { onclick: () => { state.correcting = turn.id; renderTalk(); } }, 'Correct'),
      h('button.btn.small.ghost', { onclick: () => { state.replyTo = turn.id; renderTalk(); composer?.text.focus(); } }, 'Reply'),
      h('button.btn.small.ghost', { onclick: () => select(turn.id) }, state.selected === turn.id ? 'Hide why' : 'Why?')),
    correction);
}

async function select(id) {
  if (state.selected === id) {
    state.selected = null;
    renderTalk();
    return;
  }
  state.selected = id;
  if (!state.traces.has(id)) {
    const trace = await attempt(() => get(`/v1/thoughts/${id}`));
    if (trace) state.traces.set(id, trace);
  }
  loadSignals(id);
  renderTalk();
}

async function loadSignals(id) {
  const signals = await get(`/v1/turns/${id}/signals`).catch(() => null);
  if (signals) {
    state.signals.set(id, signals);
    if (state.selected === id) renderTalk();
  }
}

function inspectorView() {
  const id = state.selected;
  const trace = id && state.traces.get(id);
  if (!trace) return null;
  const steer = (path, body, done) => attempt(() => post(`/v1/turns/${id}/${path}`, body), (r) => r?.note || done)
    .then((r) => { if (r !== undefined) loadSignals(id); });
  return inspector(trace, {
    signals: state.signals.get(id) || [],
    onPrefer: (i) => steer('prefer', { candidate: i }, 'preferred'),
    onRate: (i, rating) => steer(`candidates/${i}/rate`, { rating }, 'rated'),
    onIrrelevant: (i) => steer(`recalled/${i}/irrelevant`, {}, 'marked irrelevant'),
    onReconsider: async () => {
      const result = await attempt(() => post(`/v1/turns/${id}/reconsider`));
      if (!result) return;
      addTurn(result.aster);
      state.traces.set(result.aster.id, result.trace);
      loadSignals(id);
      state.selected = result.aster.id;
      renderTalk();
    },
    onClose: () => { state.selected = null; renderTalk(); },
  });
}

function renderTalk() {
  if (state.page !== 'talk' || !main) return;
  if (!composer) buildComposer();
  const stick = !transcriptEl || transcriptEl.scrollHeight - transcriptEl.scrollTop - transcriptEl.clientHeight < 40;
  const previousScroll = transcriptEl?.scrollTop ?? 0;
  transcriptEl = h('div.transcript', { role: 'log', 'aria-label': 'Conversation' },
    state.turns.length ? state.turns.map(messageView)
      : h('div.empty', 'No conversation yet. Say hello, or teach Aster something in Teach & ingest.'));

  const replying = state.replyTo && state.turns.find((t) => t.id === state.replyTo);
  fill(composer.el, 
    h('div.row',
      h('div.toggle', { role: 'group', 'aria-label': 'Mode' },
        h(`button${state.mode === 'chat' ? '.on' : ''}`, { onclick: () => { state.mode = 'chat'; renderTalk(); } }, 'Chat'),
        h(`button${state.mode === 'train' ? '.on' : ''}`, {
          onclick: () => { state.mode = 'train'; renderTalk(); }, title: 'Rate each reply as you go',
        }, 'Train')),
      replying && h('span.chip', `↪ replying to: ${replying.text.slice(0, 48)}`,
        h('button.btn.small.ghost', { onclick: () => { state.replyTo = null; renderTalk(); }, 'aria-label': 'Cancel reply' }, '✕')),
      h('span.grow'),
      state.mode === 'train' && h('span.small.muted', 'Rate each reply ▲ / ▼ or correct it.')),
    h('div.row', composer.text, h('button.btn.primary', { onclick: () => composer.send() }, 'Send')));

  const inspectorEl = inspectorView();
  const side = h('div.aside',
    inspectorEl,
    h('div.card', h('h3', 'Levels'), levelBars(state.levels, state.levelHistory)),
    state.inbox.length ? h('div.card', h('h3', `Inbox (${state.inbox.length})`),
      h('div.list', state.inbox.slice(0, 3).map((item) => h('div.small', item.draft.body))),
      h('button.btn.small', { onclick: () => { location.hash = 'inbox'; } }, 'Open inbox')) : null);

  fill(main, h(`div.talk${inspectorEl ? '' : '.no-inspector'}`,
    h('div.conversation', transcriptEl, composer.el), side));
  transcriptEl.scrollTop = stick ? transcriptEl.scrollHeight : previousScroll;
}

// ----- inbox ----------------------------------------------------------------

function renderInbox() {
  const items = state.inbox.map((item) => {
    const answer = h('input', { type: 'text', placeholder: 'Your answer', 'aria-label': 'Answer' });
    return h('div.card.stack',
      h('div.row', h(`span.chip${item.needs_approval ? '.warn' : '.accent'}`, item.kind),
        h('span.small.muted', when(item.created_at)), h('span.grow'),
        item.draft.subject && h('span.chip', item.draft.subject)),
      h('div', item.draft.body),
      h('div.small.muted', `why: ${item.draft.rationale}`),
      item.needs_approval
        ? h('div.row',
          h('button.btn.primary', { onclick: () => attempt(() => post(`/v1/outbound/${item.id}/approve`), 'approved').then(resync) }, 'Approve'),
          h('button.btn', { onclick: () => attempt(() => post(`/v1/outbound/${item.id}/deny`), 'denied').then(resync) }, 'Deny'))
        : h('form.row', {
          onsubmit: (event) => {
            event.preventDefault();
            const text = answer.value.trim();
            if (text) attempt(() => post(`/v1/outbound/${item.id}/answer`, { text }), (r) => r?.note || 'answered').then(resync);
          },
        }, h('div.grow', answer), h('button.btn.primary', { type: 'submit' }, 'Answer'),
        h('button.btn', { type: 'button', onclick: () => attempt(() => post(`/v1/outbound/${item.id}/deny`), 'dismissed').then(resync) }, 'Dismiss')));
  });
  fill(main, h('div.page',
    h('h2', 'Inbox'),
    h('p.muted.small', 'Questions Aster asked and requests that need your decision.'),
    items.length ? items : h('div.card.empty', 'Nothing waiting.')));
}

// ----- mind -----------------------------------------------------------------

async function renderMind() {
  const page = h('div.page', h('h2', 'Mind'));
  fill(main, page);
  const [history, runtime] = await Promise.all([
    get('/v1/state/history?limit=300').catch(() => []),
    get('/v1/runtime').catch(() => state.runtime),
  ]);
  if (state.page !== 'mind') return;
  state.runtime = runtime || state.runtime;
  const ordered = [...history].reverse();
  const t0 = ordered.length ? new Date(ordered[0].recorded_at).getTime() : 0;
  const series = LEVELS.map((name, i) => ({
    name: label(name),
    color: SERIES_COLORS[i],
    points: ordered.map((e) => [(new Date(e.recorded_at).getTime() - t0) / 60000, Number(e.state[name])]),
  }));
  const stats = Object.entries(state.runtime.mind || {});
  append(page, 
    h('div.grid2',
      h('div.card', h('h3', 'Levels now'), levelBars(state.levels, state.levelHistory)),
      h('div.card', h('h3', 'Runtime'),
        h('table', h('tbody',
          h('tr', h('th', 'engine'), h('td', state.runtime.engine || '—')),
          h('tr', h('th', 'paused'), h('td', String(!!state.runtime.paused))),
          h('tr', h('th', 'resting'), h('td', String(!!state.runtime.resting))),
          h('tr', h('th', 'idle'), h('td', `${state.runtime.idle_secs ?? 0} s`)))))),
    h('div.card', h('h3', 'Level history'),
      lineChart(series, { yMin: 0, yMax: 100, xLabel: (m) => `${Math.round(m)} min` }),
      h('div.row.small', series.map((x) => h('span.chip', { style: `color:${x.color}` }, x.name)))),
    h('div.card', h('h3', 'What Aster knows'),
      h('table', h('tbody', stats.map(([k, v]) => h('tr', h('th', label(k)), h('td.num', Number.isInteger(v) ? v : fixed(v, 3))))))),
    h('div.card', h('h3', 'Recent changes'),
      h('table', h('thead', h('tr', h('th', 'when'), h('th', 'cause'))),
        h('tbody', history.slice(0, 20).map((e) => h('tr', h('td', when(e.recorded_at)), h('td', e.cause)))))));
}

// ----- neural ---------------------------------------------------------------

let neuralView = null;

function lossPoints(history) {
  const merged = new Map(history.map((p) => [p.step, p.loss]));
  for (const p of state.training) merged.set(p.step, p.loss);
  return [...merged.entries()].sort((a, b) => a[0] - b[0]).filter(([, loss]) => Number.isFinite(loss));
}

function lossChart(points) {
  return lineChart([{ name: 'loss', color: SERIES_COLORS[0], points }], { xLabel: (x) => `step ${x}` });
}

/** Live update from a `trained` event without refetching the page. */
function updateNeural(event) {
  if (!neuralView || !neuralView.chart.isConnected) return;
  neuralView.steps.textContent = String(event.total_steps);
  neuralView.loss.textContent = fixed(event.loss, 3);
  neuralView.corpus.textContent = bytes(event.corpus_bytes);
  const chart = lossChart(lossPoints(neuralView.history));
  neuralView.chart.replaceWith(chart);
  neuralView.chart = chart;
}

async function renderNeural() {
  const page = h('div.page', h('h2', 'Neural engine'));
  fill(main, page);
  const training = await get('/v1/mind/training').catch(() => null);
  if (state.page !== 'neural' || !training) return;
  const points = lossPoints(training.history);
  const stats = training.stats;
  neuralView = {
    history: training.history,
    chart: lossChart(points),
    steps: h('td.num', stats.neural_steps ?? 0),
    loss: h('td.num', fixed(stats.neural_loss, 3)),
    corpus: h('td.num', bytes(stats.neural_corpus_bytes ?? 0)),
  };
  const neuralOn = training.engine === 'neural';
  const canNeural = training.available.includes('neural');
  const setEngine = (engine) => attempt(() => patch('/v1/config', { 'cognition.engine': engine }),
    `switching to ${engine} on the next tick`).then(() => setTimeout(() => renderNeural(), 1500));
  append(page, 
    h('div.card.stack',
      h('div.row',
        h('span', 'Engine: '), h('span.chip.accent', training.engine), h('span.grow'),
        canNeural
          ? h('button.btn', { onclick: () => setEngine(neuralOn ? 'statistical' : 'neural') },
            neuralOn ? 'Switch to statistical' : 'Switch to neural')
          : h('span.small.muted', 'This build has no neural engine (build with --features neural).')),
      h('p.small.muted', 'The neural engine keeps everything the statistical one does and adds a byte-level network, trained in the background only on what you taught. Its candidate competes with the others in every trace.')),
    neuralOn ? [
      h('div.grid2',
        h('div.card', h('h3', 'Training'),
          h('table', h('tbody',
            h('tr', h('th', 'steps'), neuralView.steps),
            h('tr', h('th', 'last loss'), neuralView.loss),
            h('tr', h('th', 'parameters'), h('td.num', (stats.neural_parameters ?? 0).toLocaleString())),
            h('tr', h('th', 'corpus'), neuralView.corpus)))),
        h('div.card', h('h3', 'Controls'),
          h('div.stack',
            h('button.btn', { onclick: () => attempt(() => patch('/v1/config', { 'cognition.neural.train_steps_per_tick': 0 }), 'training paused') }, 'Pause training'),
            h('button.btn', { onclick: () => attempt(() => patch('/v1/config', { 'cognition.neural.train_steps_per_tick': 8 }), 'training resumed') }, 'Resume training'),
            h('a.small', { href: '#config' }, 'All neural settings →')))),
      h('div.card', h('h3', 'Loss by training step'), neuralView.chart),
    ] : null);
}

// ----- memories -------------------------------------------------------------

async function renderMemories(cursor) {
  const content = h('textarea', { rows: 3, placeholder: 'Something Aster should remember', 'aria-label': 'Memory' });
  const list = h('div.list', h('div.empty', 'Loading…'));
  fill(main, h('div.page',
    h('h2', 'Memories'),
    h('form.card.stack', {
      onsubmit: async (event) => {
        event.preventDefault();
        const text = content.value.trim();
        if (!text) return;
        if (await attempt(() => post('/v1/memories', { content: text, provenance: 'owner via dashboard' }), 'remembered')) {
          renderMemories();
        }
      },
    }, content, h('div', h('button.btn.primary', { type: 'submit' }, 'Remember'))),
    list));
  const page = await attempt(() => get(`/v1/memories?limit=50${cursor ? `&before=${encodeURIComponent(cursor)}` : ''}`));
  if (!page || state.page !== 'memories') return;
  fill(list, 
    ...(page.memories.length ? page.memories.map((m) => h('div.item',
      h('div.row.small.muted', h('span.mono', m.id), h('span', when(m.created_at)), h('span', m.provenance), h('span.grow'),
        h('button.btn.small.bad', {
          onclick: () => {
            if (window.confirm('Forget this memory? Aster also unlearns it from recall.')) {
              attempt(() => del(`/v1/memories/${m.id}`), 'forgotten').then(() => renderMemories(cursor));
            }
          },
        }, 'Forget')),
      h('div', m.content))) : [h('div.empty', 'No memories yet.')]),
    page.next_cursor ? h('button.btn', { onclick: () => renderMemories(page.next_cursor) }, 'Older') : '');
}

// ----- teach & ingest ---------------------------------------------------------

async function renderTeach() {
  const prompt = h('input', { type: 'text', placeholder: 'When I say…', 'aria-label': 'Prompt' });
  const reply = h('textarea', { rows: 2, placeholder: 'Aster replies…', 'aria-label': 'Reply' });
  const files = h('div.list', h('div.empty', 'Loading…'));
  fill(main, h('div.page',
    h('h2', 'Teach & ingest'),
    h('form.card.stack', {
      onsubmit: async (event) => {
        event.preventDefault();
        if (!prompt.value.trim() || !reply.value.trim()) return;
        const result = await attempt(() => post('/v1/teach', { prompt: prompt.value.trim(), reply: reply.value.trim() }), (r) => r?.note || 'taught');
        if (result) { prompt.value = ''; reply.value = ''; prompt.focus(); }
      },
    }, h('h3', 'Teach a reply'), h('label.field', h('span', 'Prompt'), prompt), h('label.field', h('span', 'Reply'), reply),
    h('div', h('button.btn.primary', { type: 'submit' }, 'Teach'))),
    h('div.card.stack',
      h('h3', 'Ingest a file'),
      h('p.small.muted', 'Files in the server\'s ingest folder (data/ingest). Plain text becomes recall passages; .aster files are packs of meanings and replies.'),
      files)));
  const listed = await attempt(() => get('/v1/ingest/files'));
  if (!listed || state.page !== 'teach') return;
  fill(files, ...(listed.length ? [h('table',
    h('thead', h('tr', h('th', 'file'), h('th', 'size'), h('th', ''))),
    h('tbody', listed.map((f) => h('tr',
      h('td.mono', f.path),
      h('td.num', bytes(f.bytes)),
      h('td', f.too_large ? h('span.small.muted', 'too large')
        : h('button.btn.small', { onclick: () => attempt(() => post('/v1/ingest', { path: f.path }), (r) => r?.note || 'ingested') }, 'Ingest'))))))]
    : [h('div.empty', 'The ingest folder is empty. Copy files into data/ingest on the server.')]));
}

// ----- config ---------------------------------------------------------------

function flatten(value, prefix = '', out = []) {
  if (value && typeof value === 'object' && !Array.isArray(value)) {
    for (const [key, inner] of Object.entries(value)) flatten(inner, prefix ? `${prefix}.${key}` : key, out);
  } else out.push([prefix, value]);
  return out;
}

async function renderConfig() {
  const page = h('div.page', h('h2', 'Configuration'));
  fill(main, page);
  const data = await attempt(() => get('/v1/config'));
  if (!data || state.page !== 'config') return;
  const restart = data.restart_required_prefixes || [];
  const changes = new Map();
  const save = h('button.btn.primary', { disabled: true }, 'Save changes');
  const counter = h('span.small.muted', '');
  const update = () => {
    save.disabled = changes.size === 0;
    counter.textContent = changes.size ? `${changes.size} unsaved` : '';
  };
  save.onclick = async () => {
    const body = Object.fromEntries(changes);
    const result = await attempt(() => patch('/v1/config', body));
    if (!result) return;
    toast(result.restart_required.length ? `saved; restart needed for ${result.restart_required.join(', ')}` : 'saved and applied live');
    renderConfig();
  };
  const sections = {};
  for (const [key, value] of flatten(data.config)) {
    const section = key.split('.')[0];
    (sections[section] ||= []).push([key, value]);
  }
  const rowFor = ([key, value]) => {
    const row = h('div.cfg-row');
    const mark = (next) => {
      if (JSON.stringify(next) === JSON.stringify(value)) changes.delete(key);
      else changes.set(key, next);
      row.classList.toggle('changed', changes.has(key));
      update();
    };
    let input;
    if (typeof value === 'boolean') {
      input = h('input', { type: 'checkbox', checked: value, onchange: (e) => mark(e.target.checked), 'aria-label': key });
    } else if (typeof value === 'number') {
      input = h('input', {
        type: 'number', value: String(value), step: 'any', 'aria-label': key,
        oninput: (e) => { if (e.target.value !== '' && Number.isFinite(Number(e.target.value))) mark(Number(e.target.value)); },
      });
    } else if (typeof value === 'string' || value === null) {
      input = h('input', { type: 'text', value: value ?? '', 'aria-label': key, oninput: (e) => mark(e.target.value) });
    } else {
      input = h('input.mono', {
        type: 'text', value: JSON.stringify(value), 'aria-label': key,
        oninput: (e) => { try { mark(JSON.parse(e.target.value)); e.target.setCustomValidity(''); } catch { e.target.setCustomValidity('JSON'); } },
      });
    }
    const needsRestart = restart.some((p) => key.startsWith(p));
    append(row, h('div.key', key.split('.').slice(1).join('.') || key, needsRestart ? h('span.chip.warn', 'restart') : null), input);
    return row;
  };
  append(page, 
    h('div.card.row', h('span.small.muted.grow', `Live from ${data.path}. Changes are validated before they apply.`), counter, save),
    ...Object.entries(sections).map(([name, rows]) => h('div.card.cfg-section', h('h3', name), rows.map(rowFor))));
}

// ----- snapshots ------------------------------------------------------------

async function renderSnapshots() {
  const reason = h('input', { type: 'text', placeholder: 'reason (optional)', 'aria-label': 'Snapshot reason', maxlength: 200 });
  const list = h('div', h('div.empty', 'Loading…'));
  fill(main, h('div.page',
    h('h2', 'Snapshots'),
    h('form.card.row', {
      onsubmit: async (event) => {
        event.preventDefault();
        const body = reason.value.trim() ? { reason: reason.value.trim() } : {};
        if (await attempt(() => post('/v1/mind/snapshots', body), (r) => `saved v${r.version}`)) renderSnapshots();
      },
    }, h('div.grow', reason), h('button.btn.primary', { type: 'submit' }, 'Save snapshot')),
    h('div.card', list)));
  const snapshots = await attempt(() => get('/v1/mind/snapshots?limit=50'));
  if (!snapshots || state.page !== 'snapshots') return;
  fill(list, h('table',
    h('thead', h('tr', h('th', 'version'), h('th', 'engine'), h('th', 'reason'), h('th', 'when'), h('th', ''))),
    h('tbody', snapshots.map((x) => h('tr',
      h('td.mono', `v${x.version}`), h('td', x.engine), h('td', x.reason), h('td', when(x.created_at)),
      h('td', h('button.btn.small', {
        onclick: () => {
          if (window.confirm(`Roll Aster's mind back to v${x.version}? Learning since then is replaced (it stays in older snapshots).`)) {
            attempt(() => post('/v1/mind/rollback', { version: x.version }), 'rolled back').then(() => renderSnapshots());
          }
        },
      }, 'Roll back')))))));
}

// ----- logs -----------------------------------------------------------------

async function renderLogs(category = 'conversations') {
  const select = h('select', { 'aria-label': 'Log category', onchange: (e) => renderLogs(e.target.value) },
    LOG_CATEGORIES.map((c) => h('option', { value: c, selected: c === category }, c)));
  const output = h('pre.log', 'Loading…');
  fill(main, h('div.page',
    h('h2', 'Logs'),
    h('div.card.row', h('span.small.muted.grow', 'Newest JSONL entries from the server\'s log folder.'), select,
      h('button.btn', { onclick: () => renderLogs(category) }, 'Refresh')),
    h('div.card', output)));
  const entries = await attempt(() => get(`/v1/logs/${category}?limit=200`));
  if (!entries || state.page !== 'logs') return;
  output.textContent = entries.length ? entries.map((e) => JSON.stringify(e)).join('\n') : 'No entries yet.';
  output.scrollTop = output.scrollHeight;
}

start();
