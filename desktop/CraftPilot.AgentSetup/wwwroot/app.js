const token = document.querySelector('meta[name="cp-token"]')?.content ?? '';
const $ = (id) => document.getElementById(id);
const state = { busy: false, loaded: false, dockerReady: false, installed: false, agentExists: false, updating: false };

async function api(path, options = {}) {
  const response = await fetch(path, {
    ...options,
    headers: {
      'Content-Type': 'application/json',
      'X-CraftPilot-Token': token,
      ...(options.headers ?? {}),
    },
  });

  const data = await response.json().catch(() => ({}));
  if (!response.ok) {
    throw new Error(data.error || `HTTP ${response.status}`);
  }

  return data;
}

function setDot(id, kind) {
  const dot = $(id);
  dot.className = `dot dot-${kind}`;
}

function setBadge(id, text, kind) {
  const badge = $(id);
  badge.textContent = text;
  badge.className = `badge badge-${kind}`;
}

function setCard(cardId, dotId, badgeId, titleId, detailId, kind, badge, title, detail) {
  setDot(dotId, kind);
  setBadge(badgeId, badge, kind);
  $(titleId).textContent = title;
  $(detailId).textContent = detail;
  $(cardId).dataset.status = kind;
}

function versionLabel(value) {
  if (!value) {
    return 'v?';
  }

  return /^v/i.test(value) ? value : `v${value}`;
}

function renderVersion(check) {
  const current = versionLabel(check.currentVersion);
  const pill = $('panel-version');
  const banner = $('update-banner');
  const link = $('update-link');

  pill.textContent = current;
  pill.dataset.state = check.error ? 'error' : 'ok';
  pill.title = check.error
    ? `Panelversion ${current}. ${check.error}`
    : `Panelversion ${current}`;

  if (check.updateAvailable && check.latestVersion && check.releaseUrl) {
    banner.hidden = false;
    banner.dataset.state = 'available';
    $('update-dot').className = 'dot dot-warn';
    $('update-title').textContent = `Ny panelversion ${versionLabel(check.latestVersion)} finns`;
    $('update-text').textContent = `Du kör ${current}. Panelen hämtar filen från GitHub, kontrollerar kontrollsumman och startar om sig själv.`;
    link.href = check.releaseUrl;
    link.hidden = false;
    $('update-now').hidden = false;
    return;
  }

  if (check.error) {
    banner.hidden = false;
    banner.dataset.state = 'error';
    $('update-dot').className = 'dot dot-idle';
    $('update-title').textContent = 'Kunde inte kontrollera panelversionen';
    $('update-text').textContent = `${check.error} Klicka på Kontrollera igen för att försöka på nytt.`;
    link.hidden = true;
    $('update-now').hidden = true;
    return;
  }

  banner.hidden = true;
  link.hidden = true;
  $('update-now').hidden = true;
}

async function checkVersion() {
  try {
    renderVersion(await api('/api/version'));
  } catch (error) {
    renderVersion({ error: `Kunde inte läsa versionsinformationen: ${error.message}` });
  }
}

function shortError(value) {
  if (!value) {
    return 'Ingen detalj tillgänglig.';
  }

  return value.split('\n').map((line) => line.trim()).find(Boolean) ?? 'Okänt fel.';
}

function setDockerNotice(status, dockerState) {
  const notice = $('docker-notice');
  const download = $('docker-download');
  const cardLink = $('docker-link');

  if (dockerState === 'ready') {
    notice.hidden = true;
    download.hidden = true;
    cardLink.hidden = true;
    return;
  }

  const notInstalled = dockerState === 'not-installed';
  const desktopPresent = status.dockerDesktopInstalled === true;
  notice.hidden = false;

  $('docker-notice-title').textContent = notInstalled
    ? (desktopPresent ? 'Docker-kommandot hittades inte' : 'Docker Desktop saknas')
    : (dockerState === 'not-running' ? 'Docker Desktop är inte igång' : 'Docker svarar inte');

  $('docker-notice-text').textContent = notInstalled
    ? (desktopPresent
      ? 'Docker Desktop finns men kommandot docker hittades inte. Starta om Docker Desktop och panelen.'
      : 'Installera Docker Desktop med Linux-containrar, starta det och klicka på Kontrollera igen.')
    : (dockerState === 'not-running'
      ? 'Starta Docker Desktop och vänta tills motorn är redo. Installationen är blockerad tills dess.'
      : 'Kontrollera Docker Desktop och klicka på Kontrollera igen.');

  download.hidden = !notInstalled || desktopPresent;
  cardLink.hidden = !notInstalled || desktopPresent;
}

function renderStatus(status) {
  state.loaded = true;
  const dockerState = status.dockerState || (status.dockerOk ? 'ready' : 'error');
  state.dockerReady = dockerState === 'ready';
  state.installed = Boolean(status.installed);
  state.agentExists = Boolean(status.agentExists);

  if (state.dockerReady) {
    setCard(
      'docker-card', 'docker-dot', 'docker-badge', 'docker-title', 'docker-detail',
      'ok', 'Redo',
      'Docker är redo',
      `Server ${status.dockerVersion || 'tillgänglig'}`
    );
  } else if (dockerState === 'not-installed') {
    setCard(
      'docker-card', 'docker-dot', 'docker-badge', 'docker-title', 'docker-detail',
      'warn', 'Saknas',
      status.dockerDesktopInstalled ? 'Docker-kommandot saknas' : 'Docker Desktop saknas',
      status.dockerDesktopInstalled
        ? 'Docker Desktop finns men kommandot docker hittades inte. Starta om Docker Desktop och panelen.'
        : 'Installera Docker Desktop med Linux-containrar för att kunna installera agenten.'
    );
  } else if (dockerState === 'not-running') {
    setCard(
      'docker-card', 'docker-dot', 'docker-badge', 'docker-title', 'docker-detail',
      'warn', 'Stoppad',
      'Docker Desktop är inte igång',
      'Starta Docker Desktop och vänta tills motorn är redo.'
    );
  } else {
    setCard(
      'docker-card', 'docker-dot', 'docker-badge', 'docker-title', 'docker-detail',
      'bad', 'Fel',
      'Docker svarar inte',
      shortError(status.dockerError)
    );
  }

  setDockerNotice(status, dockerState);

  if (status.masterOk) {
    const latency = status.masterLatencyMs != null ? ` · ${status.masterLatencyMs} ms` : '';
    const version = status.masterVersion
      ? (/^v/i.test(status.masterVersion) ? status.masterVersion : `v${status.masterVersion}`)
      : 'Hälsokontroll OK';
    setCard(
      'master-card', 'master-dot', 'master-badge', 'master-title', 'master-detail',
      'ok', 'Online',
      'Mastern nås',
      `${version}${latency}`
    );
  } else {
    setCard(
      'master-card', 'master-dot', 'master-badge', 'master-title', 'master-detail',
      'bad', 'Fel',
      'Mastern svarar inte',
      shortError(status.masterError)
    );
  }

  if (!state.dockerReady) {
    setCard(
      'agent-card', 'agent-dot', 'agent-badge', 'agent-title', 'agent-detail',
      'idle', 'Väntar',
      'Kan inte kontrollera agenten',
      'Docker måste vara igång innan agenten kan kontrolleras.'
    );
  } else if (status.agentRunning) {
    setCard(
      'agent-card', 'agent-dot', 'agent-badge', 'agent-title', 'agent-detail',
      'ok', 'Körs',
      'Agenten är igång',
      status.agentImage || 'Containern körs.'
    );
  } else if (status.installed) {
    setCard(
      'agent-card', 'agent-dot', 'agent-badge', 'agent-title', 'agent-detail',
      'warn', 'Stoppad',
      'Agenten är stoppad',
      'Containern finns men körs inte just nu.'
    );
  } else if (status.agentExists) {
    setCard(
      'agent-card', 'agent-dot', 'agent-badge', 'agent-title', 'agent-detail',
      'warn', 'Hittad',
      'Agentcontainern finns redan',
      'Panelen saknar installationsfilerna. Klicka på Installera / starta för att ta över den, eller Ta bort.'
    );
  } else {
    setCard(
      'agent-card', 'agent-dot', 'agent-badge', 'agent-title', 'agent-detail',
      'idle', 'Saknas',
      'Ingen agent installerad',
      'Välj serverdata och klicka på Installera / starta.'
    );
  }

  if (!state.dockerReady) {
    setCard(
      'claim-card', 'claim-dot', 'claim-badge', 'claim-title', 'claim-detail',
      'idle', 'Väntar',
      'Claim-kod',
      'Koden visas när Docker och agenten är igång.'
    );
    $('claim-code').textContent = '—';
    $('copy-claim').disabled = true;
  } else if (status.enrollmentCompleted) {
    setCard(
      'claim-card', 'claim-dot', 'claim-badge', 'claim-title', 'claim-detail',
      'ok', 'Klar',
      'Redan ansluten',
      'Agenten har slutfört sin anslutning till mastern.'
    );
    $('claim-code').textContent = 'Ansluten';
    $('copy-claim').disabled = true;
  } else if (status.claimCode) {
    setCard(
      'claim-card', 'claim-dot', 'claim-badge', 'claim-title', 'claim-detail',
      'warn', 'Väntar',
      'Anslut agenten',
      'Kopiera koden och klistra in den under Min nod.'
    );
    $('claim-code').textContent = status.claimCode;
    $('copy-claim').disabled = false;
  } else {
    setCard(
      'claim-card', 'claim-dot', 'claim-badge', 'claim-title', 'claim-detail',
      'idle', 'Väntar',
      'Claim-kod',
      'Koden visas när agenten har startat.'
    );
    $('claim-code').textContent = '—';
    $('copy-claim').disabled = true;
  }

  if (document.activeElement !== $('data-dir')) {
    $('data-dir').value = status.dataDir || '';
  }
  if (document.activeElement !== $('startup')) {
    $('startup').checked = Boolean(status.startupEnabled);
  }

  $('install-dir').textContent = status.installDir || '—';
  $('master-link').href = 'https://craftpilot.se/node';
  $('settings-note').textContent = status.busy ? 'Arbetar…' : 'Master låst';

  renderServers(status);

  state.busy = Boolean(status.busy);
  applyControls();
}

async function refresh(includeLogs = false) {
  try {
    const status = await api('/api/status');
    renderStatus(status);
    if (includeLogs) {
      await loadLogs();
    }
  } catch (error) {
    appendLog(`Kunde inte läsa status: ${error.message}`);
  }
}

async function loadLogs() {
  try {
    const result = await api('/api/logs');
    $('log').textContent = result.logs || 'Ingen agentlogg ännu.';
    $('log').scrollTop = $('log').scrollHeight;
  } catch (error) {
    appendLog(`Kunde inte läsa loggen: ${error.message}`);
  }
}

function appendLog(message) {
  const log = $('log');
  const current = log.textContent === 'Startar panelen…' ? '' : log.textContent;
  log.textContent = `${current}${current ? '\n' : ''}${message}`;
  log.scrollTop = log.scrollHeight;
}

function applyControls() {
  const canInstall = state.dockerReady && !state.busy;
  const canManage = state.installed && state.dockerReady && !state.busy;

  for (const id of ['install', 'install-panel']) {
    $(id).disabled = !canInstall;
  }
  $('update').disabled = !canManage;

  // Stoppa och Ta bort gäller även en container som panelen inte installerat
  // själv (t.ex. efter en tidigare installation eller start-rs-agent.ps1).
  const canManageContainer = (state.installed || state.agentExists) && state.dockerReady && !state.busy;
  $('stop').disabled = !canManageContainer;
  $('remove').disabled = !canManageContainer;
  $('cancel').disabled = !state.busy;
  $('refresh').disabled = state.busy;
  $('open-folder').disabled = state.busy;
  $('startup').disabled = state.busy;
  $('load-logs').disabled = state.busy || !state.installed;
  $('prune-servers').disabled = state.busy || !state.dockerReady;
  $('update-now').disabled = state.busy;
}

const TAB_IDS = ['main', 'settings', 'log'];
const TAB_STORAGE_KEY = 'craftpilot-tab';

// Byter flik och speglar valet i aria-attributen, så både mus, tangentbord
// och skärmläsare hamnar på samma ställe.
function selectTab(name, options = {}) {
  const target = TAB_IDS.includes(name) ? name : 'main';

  for (const tab of TAB_IDS) {
    const button = $(`tab-button-${tab}`);
    const panel = $(`tab-${tab}`);
    if (!button || !panel) {
      continue;
    }

    const selected = tab === target;
    button.setAttribute('aria-selected', selected ? 'true' : 'false');
    button.tabIndex = selected ? 0 : -1;
    panel.hidden = !selected;
  }

  try {
    window.localStorage.setItem(TAB_STORAGE_KEY, target);
  } catch {
    // localStorage kan vara avstängt i WebView2; fliken fungerar ändå.
  }

  if (options.focus) {
    $(`tab-button-${target}`)?.focus();
  }
}

function setupTabs() {
  for (const tab of TAB_IDS) {
    const button = $(`tab-button-${tab}`);
    if (!button) {
      continue;
    }

    button.addEventListener('click', () => selectTab(tab));
    button.addEventListener('keydown', (event) => {
      const index = TAB_IDS.indexOf(tab);
      if (event.key === 'ArrowRight' || event.key === 'ArrowLeft') {
        event.preventDefault();
        const step = event.key === 'ArrowRight' ? 1 : TAB_IDS.length - 1;
        selectTab(TAB_IDS[(index + step) % TAB_IDS.length], { focus: true });
      } else if (event.key === 'Home' || event.key === 'End') {
        event.preventDefault();
        selectTab(event.key === 'Home' ? TAB_IDS[0] : TAB_IDS[TAB_IDS.length - 1], { focus: true });
      }
    });
  }

  let saved = null;
  try {
    saved = window.localStorage.getItem(TAB_STORAGE_KEY);
  } catch {
    // Utan sparat val börjar panelen på Översikt.
  }

  selectTab(saved ?? 'main');
}

const SERVER_STATES = {
  running: { label: 'Körs', kind: 'ok' },
  restarting: { label: 'Startar om', kind: 'warn' },
  paused: { label: 'Pausad', kind: 'warn' },
  exited: { label: 'Stoppad', kind: 'idle' },
  created: { label: 'Skapad', kind: 'idle' },
  dead: { label: 'Kraschad', kind: 'bad' },
};

function serverRow(server) {
  const info = SERVER_STATES[server.state] ?? {
    label: server.state ? server.state : 'Okänd',
    kind: 'idle',
  };

  const item = document.createElement('li');
  item.className = 'server-row';

  const dot = document.createElement('span');
  dot.className = `dot dot-${info.kind}`;
  dot.setAttribute('aria-hidden', 'true');

  const main = document.createElement('div');
  main.className = 'server-main';

  const name = document.createElement('span');
  name.className = 'server-name';
  name.textContent = `Server ${server.shortId || server.serverId}`;

  const image = document.createElement('span');
  image.className = 'server-image';
  image.textContent = `${server.name} · ${server.image}`;

  main.append(name, image);

  const badge = document.createElement('span');
  badge.className = `badge badge-${info.kind}`;
  badge.textContent = info.label;

  item.append(dot, main, badge);
  return item;
}

// Serverlistan kommer från agentens Docker-containrar (label craftpilot.managed),
// så den visar vad noden faktiskt hostar just nu.
function renderServers(status) {
  const list = $('servers-list');
  const empty = $('servers-empty');
  const count = $('servers-count');
  if (!list || !empty || !count) {
    return;
  }

  const servers = Array.isArray(status.servers) ? status.servers : [];
  const running = servers.filter((server) => server.state === 'running' || server.running).length;

  count.textContent = `${running}/${servers.length}`;
  count.title = `${running} av ${servers.length} servrar körs`;
  count.className = servers.length === 0
    ? 'badge badge-idle'
    : running === servers.length
      ? 'badge badge-ok'
      : 'badge badge-warn';

  list.replaceChildren(...servers.map(serverRow));
  list.hidden = servers.length === 0;
  empty.hidden = servers.length > 0;

  const stopped = servers.filter((server) => !server.running && server.state !== 'running').length;
  const prune = $('prune-servers');
  if (prune) {
    prune.hidden = stopped === 0;
    prune.textContent = stopped === 1 ? 'Rensa 1 stoppad' : `Rensa ${stopped} stoppade`;
  }
  empty.textContent = status.dockerOk === false
    ? 'Serverlistan kan inte läsas förrän Docker Desktop är igång.'
    : 'Här listas de servrar som agenten kör på den här datorn. Skapa en server i panelen på craftpilot.se – så dyker den upp här med status.';
}

function setBusy(busy) {
  state.busy = busy;
  document.body.classList.toggle('is-busy', busy);
  applyControls();
}

let progressTimer = null;
let progressStartedAt = 0;

// Panelen får inget delresultat från agenten, så baren är obestämd och visar
// i stället hur länge åtgärden har pågått. Fliken byts aldrig automatiskt.
function startProgress(label) {
  const bar = $('progress');
  const track = $('progress-track');
  if (!bar || !track) {
    return;
  }

  progressStartedAt = Date.now();
  $('progress-label').textContent = label;
  $('progress-elapsed').textContent = '0 s';
  track.setAttribute('aria-valuetext', label);
  track.removeAttribute('aria-valuenow');
  track.style.removeProperty('--progress');
  bar.dataset.mode = 'indeterminate';
  bar.hidden = false;

  window.clearInterval(progressTimer);
  progressTimer = window.setInterval(() => {
    if (bar.dataset.mode === 'determinate') {
      return;
    }

    const seconds = Math.max(0, Math.round((Date.now() - progressStartedAt) / 1000));
    $('progress-elapsed').textContent = `${seconds} s`;
    track.setAttribute('aria-valuetext', `${label}, ${seconds} sekunder`);
  }, 250);
}

function stopProgress() {
  window.clearInterval(progressTimer);
  progressTimer = null;
  const bar = $('progress');
  if (bar) {
    bar.hidden = true;
    bar.dataset.mode = 'indeterminate';
  }
}

// Panelens nedladdning vet hur många byte som kommit, så baren kan fyllas i
// procent. Docker-åtgärderna har ingen delprogress och är kvar obestämda.
function setProgressDeterminate(percent) {
  const bar = $('progress');
  const track = $('progress-track');
  if (!bar || !track) {
    return;
  }

  const value = Math.max(0, Math.min(100, Math.round(percent)));
  const label = $('progress-label').textContent;
  bar.dataset.mode = 'determinate';
  track.style.setProperty('--progress', `${value}%`);
  track.setAttribute('aria-valuemin', '0');
  track.setAttribute('aria-valuemax', '100');
  track.setAttribute('aria-valuenow', String(value));
  track.setAttribute('aria-valuetext', `${label}, ${value} procent`);
  $('progress-elapsed').textContent = `${value} %`;
}

async function refreshDownloadProgress() {
  try {
    const progress = await api('/api/panel-update/progress');
    if (progress.active && progress.percent != null) {
      setProgressDeterminate(progress.percent);
    }
  } catch {
    // Panelen kan redan ha stängt servern; baren städas i runPanelUpdate.
  }
}

async function runOperation(path, body, label) {
  if (state.busy) {
    return;
  }

  if (!state.dockerReady) {
    appendLog('Docker Desktop måste vara igång innan den här åtgärden kan köras.');
    return;
  }

  setBusy(true);
  startProgress(label);
  appendLog(`\n> ${label}…`);

  try {
    const result = await api(path, {
      method: 'POST',
      body: JSON.stringify(body ?? {}),
    });

    if (result.output) {
      $('log').textContent = result.output;
    } else {
      appendLog(result.ok ? 'Klart.' : 'Åtgärden misslyckades.');
    }

    if (!result.ok && result.error) {
      appendLog(`Fel: ${result.error}`);
    }

    if (result.status) {
      renderStatus(result.status);
    } else {
      await refresh(false);
    }
  } catch (error) {
    appendLog(`Fel: ${error.message}`);
  } finally {
    stopProgress();
    setBusy(false);
  }
}

function install() {
  if (!state.dockerReady) {
    appendLog('Docker Desktop måste vara igång innan agenten kan installeras.');
    return;
  }

  const dataDir = $('data-dir').value.trim();
  runOperation('/api/install', { dataDir }, 'Installerar och startar agenten');
}

$('install').addEventListener('click', install);
$('install-panel').addEventListener('click', install);
// Hämtar panelens nästa version från GitHub, byter ut exen och startar om
// panelen. Den här sidan stängs när den nya panelen tar över.
async function runPanelUpdate() {
  if (state.busy) {
    return;
  }

  if (!window.confirm('Ladda ner ny panelversion nu? Panelen startar om när filen är utbytt.')) {
    return;
  }

  setBusy(true);
  state.updating = true;
  startProgress('Laddar ner ny panelversion');
  $('update-text').textContent = 'Laddar ner och kontrollerar den nya versionen…';
  $('update-now').hidden = true;
  const progressPoll = window.setInterval(refreshDownloadProgress, 400);

  try {
    const result = await api('/api/panel-update', { method: 'POST', body: '{}' });
    if (!result.ok) {
      state.updating = false;
      $('update-text').textContent = result.error ?? 'Uppdateringen misslyckades.';
      $('update-now').hidden = false;
      appendLog(`Uppdateringen misslyckades: ${result.error ?? 'okänt fel'}`);
      return;
    }

    $('update-dot').className = 'dot dot-ok';
    $('update-title').textContent = `Panel ${versionLabel(result.version)} är installerad`;
    $('update-text').textContent = 'Panelen startar om…';
    appendLog(`Panel ${versionLabel(result.version)} installerad. Panelen startar om.`);
  } catch (error) {
    state.updating = false;
    $('update-text').textContent = `Uppdateringen misslyckades: ${error.message}`;
    $('update-now').hidden = false;
  } finally {
    window.clearInterval(progressPoll);
    stopProgress();
    if (!state.updating) {
      setBusy(false);
    }
  }
}

$('update-now').addEventListener('click', runPanelUpdate);
$('refresh').addEventListener('click', () => {
  refresh(true);
  checkVersion();
});
$('load-logs').addEventListener('click', loadLogs);
$('show-log').addEventListener('click', () => selectTab('log'));

// Städar bort servercontainrar som inte körs. Servernas filer ligger kvar på
// disken, så en start i mastern skapar en ny container med samma data.
$('prune-servers').addEventListener('click', () => {
  const message = 'Ta bort de stoppade servercontainrarna? Servernas filer ligger kvar på disken.';
  if (!window.confirm(message)) {
    return;
  }

  runOperation('/api/servers/prune', {}, 'Rensar stoppade servercontainrar');
});

$('startup').addEventListener('change', async () => {
  const enabled = $('startup').checked;
  try {
    const result = await api('/api/startup', {
      method: 'POST',
      body: JSON.stringify({ enabled }),
    });
    if (!result.ok) {
      throw new Error(result.error || 'Kunde inte ändra automatisk start.');
    }
    appendLog(enabled ? 'Panelen startar med Windows.' : 'Automatisk start avstängd.');
  } catch (error) {
    $('startup').checked = !enabled;
    appendLog(`Kunde inte ändra automatisk start: ${error.message}`);
  }
});

$('update').addEventListener('click', () => {
  runOperation('/api/update', null, 'Hämtar senaste agentimagen');
});

$('stop').addEventListener('click', () => {
  if (!window.confirm('Stoppa agenten? Servrarna behålls och kan startas igen senare.')) {
    return;
  }
  runOperation('/api/stop', null, 'Stoppar agenten');
});

$('cancel').addEventListener('click', async () => {
  if (!state.busy) {
    return;
  }

  try {
    const result = await api('/api/cancel', { method: 'POST', body: '{}' });
    appendLog(result.ok ? 'Avbryter den pågående åtgärden…' : 'Ingen pågående åtgärd att avbryta.');
  } catch (error) {
    appendLog(`Kunde inte avbryta: ${error.message}`);
  }
});

$('remove').addEventListener('click', () => {
  if (!window.confirm('Ta bort agenten från datorn? Containern tas bort och agenten kopplas bort från noden. Serverdata och agentens state ligger kvar.')) {
    return;
  }
  runOperation('/api/remove', null, 'Tar bort agenten');
});

$('open-folder').addEventListener('click', async () => {
  try {
    await api('/api/open-folder', { method: 'POST', body: '{}' });
  } catch (error) {
    appendLog(`Kunde inte öppna mappen: ${error.message}`);
  }
});

$('copy-claim').addEventListener('click', async () => {
  const code = $('claim-code').textContent.trim();
  if (!code || code === '—') {
    return;
  }

  try {
    await navigator.clipboard.writeText(code);
    const button = $('copy-claim');
    const previous = button.textContent;
    button.textContent = 'Kopierad';
    window.setTimeout(() => { button.textContent = previous; }, 1400);
  } catch {
    appendLog('Kunde inte kopiera automatiskt. Markera koden manuellt.');
  }
});

$('quit').addEventListener('click', async () => {
  if (!window.confirm('Stäng panelen? Agenten fortsätter köra i Docker.')) {
    return;
  }

  try {
    await api('/api/quit', { method: 'POST', body: '{}' });
  } catch {
    // The server may close before the response is read.
  }
  window.close();
});

setupTabs();
refresh(true);
checkVersion();
window.setInterval(() => {
  if (!state.busy) {
    refresh(false);
  }
}, 5000);
