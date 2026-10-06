// Popover logic: renders known lights and forwards changes to the Rust side.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (sel) => document.querySelector(sel);
const card = $('#card');
const list = $('#lights');
const empty = $('#empty');
const status = $('#status');
const tpl = $('#light-tpl');

const TEMP_MIN = 143; // mireds (7000 K)
const TEMP_MAX = 344; // mireds (2900 K)
const miredToKelvin = (m) => Math.round(1e6 / m / 50) * 50;
const kelvinToMired = (k) => Math.min(TEMP_MAX, Math.max(TEMP_MIN, Math.round(1e6 / k)));

let lights = [];
const cards = new Map(); // id -> DOM node (id is the serial, or the IP until it is known)
const pending = new Map(); // id -> { patch, inflight, timer }
let scanning = false;

// ---------- status ----------

function setStatus(text, isError = false) {
  status.textContent = text || '';
  status.hidden = !text;
  status.classList.toggle('error', isError);
}

function setScanning(on) {
  scanning = on;
  $('#rescan').classList.toggle('busy', on);
  if (on) setStatus('Searching the network…');
  else if (status.textContent.startsWith('Searching')) setStatus('');
}

// ---------- rendering ----------

function isDragging(node, cls) {
  const el = node.querySelector(cls);
  return document.activeElement === el && el.matches(':active');
}

function applyValues(node, light) {
  node.classList.toggle('off', !light.on);
  node.classList.toggle('offline', !light.reachable);
  node.querySelector('.name').textContent = light.name || `Key Light (${light.ip})`;
  const product = light.product || 'Elgato light';
  node.querySelector('.sub-text').textContent =
    `${product} · ${light.ip}${light.reachable ? '' : ' · offline'}`;
  node.querySelector('.power').checked = !!light.on;
  // Offline sliders ignore the mouse (CSS); disabling them keeps Tab from stopping there too.
  node.querySelector('.bri').disabled = !light.reachable;
  node.querySelector('.temp').disabled = !light.reachable;

  const hasPending = pending.has(light.id) && Object.keys(pending.get(light.id).patch).length > 0;
  if (!hasPending && !isDragging(node, '.bri')) {
    node.querySelector('.bri').value = light.brightness;
    node.querySelector('.bri-val').textContent = `${light.brightness}%`;
  }
  if (!hasPending && !isDragging(node, '.temp')) {
    const k = miredToKelvin(light.temperature);
    node.querySelector('.temp').value = k;
    node.querySelector('.temp-val').textContent = `${k}K`;
  }
}

function buildCard(light) {
  const node = tpl.content.firstElementChild.cloneNode(true);
  node.dataset.id = light.id;

  node.querySelector('.power').addEventListener('change', (e) => {
    const on = e.target.checked;
    node.classList.toggle('off', !on);
    send(light.id, { on });
  });

  const bri = node.querySelector('.bri');
  bri.addEventListener('input', () => {
    node.querySelector('.bri-val').textContent = `${bri.value}%`;
    send(light.id, { brightness: Number(bri.value) });
  });

  const temp = node.querySelector('.temp');
  temp.addEventListener('input', () => {
    node.querySelector('.temp-val').textContent = `${temp.value}K`;
    send(light.id, { temperature: kelvinToMired(Number(temp.value)) });
  });

  node.querySelector('.remove').addEventListener('click', async () => {
    // Keep keyboard focus nearby instead of losing it with the removed card.
    const next = node.nextElementSibling || node.previousElementSibling;
    const hadFocus = node.contains(document.activeElement);
    lights = await invoke('remove_light', { id: light.id });
    render();
    if (hadFocus) (next?.querySelector('.power') || $('#rescan')).focus();
  });

  return node;
}

function render() {
  const seen = new Set();
  for (const light of lights) {
    seen.add(light.id);
    let node = cards.get(light.id);
    if (!node) {
      node = buildCard(light);
      cards.set(light.id, node);
    }
    applyValues(node, light);
    list.appendChild(node); // appending re-orders to match the sorted list
  }
  for (const [id, node] of cards) {
    if (!seen.has(id)) {
      node.remove();
      cards.delete(id);
    }
  }
  empty.hidden = lights.length > 0 || scanning;
}

// ---------- sending changes (coalesced, one request in flight per light) ----------

function send(id, patch) {
  const p = pending.get(id) || { patch: {}, inflight: false, timer: null };
  Object.assign(p.patch, patch);
  pending.set(id, p);
  if (!p.timer) p.timer = setTimeout(() => flush(id), 40);
}

async function flush(id) {
  const p = pending.get(id);
  if (!p) return;
  p.timer = null;
  if (p.inflight) return; // flush() runs again when the current request finishes
  const patch = p.patch;
  p.patch = {};
  if (Object.keys(patch).length === 0) return;
  p.inflight = true;
  try {
    const updated = await invoke('set_light', { id, patch });
    const idx = lights.findIndex((l) => l.id === id);
    if (idx >= 0) lights[idx] = updated;
    const node = cards.get(id);
    if (node) applyValues(node, updated);
    setStatus('');
  } catch (err) {
    const l = lights.find((x) => x.id === id);
    setStatus(`Could not reach ${l ? l.name || l.ip : 'the light'}: ${err}`, true);
    if (l) { l.reachable = false; const node = cards.get(id); if (node) applyValues(node, l); }
  } finally {
    p.inflight = false;
    if (Object.keys(p.patch).length) flush(id);
  }
}

// ---------- refresh / scan / add ----------

async function refresh() {
  try {
    lights = await invoke('refresh');
    render();
  } catch (err) {
    setStatus(String(err), true);
  }
}

async function rescan() {
  if (scanning) return;
  setScanning(true);
  try {
    lights = await invoke('rescan');
  } catch (err) {
    setStatus(String(err), true);
  } finally {
    setScanning(false);
    render();
  }
}

$('#rescan').addEventListener('click', rescan);

$('#add-toggle').addEventListener('click', () => {
  const row = $('#add-row');
  row.hidden = !row.hidden;
  if (!row.hidden) $('#add-ip').focus();
});

$('#add-row').addEventListener('submit', async (e) => {
  e.preventDefault();
  const input = $('#add-ip');
  const ip = input.value.trim();
  if (!ip) return;
  setStatus(`Checking ${ip}…`);
  try {
    lights = await invoke('add_light', { ip });
    input.value = '';
    $('#add-row').hidden = true;
    setStatus('');
    render();
  } catch (err) {
    setStatus(String(err), true);
  }
});

// ---------- window plumbing ----------

// Keep the native window sized to the content (card + its 12px shadow margin).
const ro = new ResizeObserver(() => {
  const h = card.offsetHeight + 24;
  invoke('resize_popover', { height: h });
});
ro.observe(card);

document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') invoke('close_popover');
});
document.addEventListener('contextmenu', (e) => e.preventDefault());

listen('lights', (e) => { lights = e.payload; render(); });
listen('scanning', (e) => { setScanning(!!e.payload); render(); });
listen('popover-shown', () => refresh());

(async () => {
  lights = await invoke('list_lights');
  render();
  refresh();
})();
