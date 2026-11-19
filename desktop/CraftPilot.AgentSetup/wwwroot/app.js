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

function setCard(cardId, dotId, badgeId, valueId, kind, badge, value, title) {
  setDot(dotId, kind);
  setBadge(badgeId, badge, kind);

  // Kortet har etikett, badge och ett värde — ingen löptext. Hela felet
  // ligger i title när värdet är en förkortad rad.
  const node = $(valueId);
  node.textContent = value;
  if (title) {
    node.title = title;
  } else {
    node.removeAttribute('title');
  }

  $(cardId).dataset.status = kind;
}

function versionLabel(value) {
  if (!value) {
    return 'v?';
  }

  return /^v/i.test(value) ? value : `v${value}`;
}

// Agentimagen är lång (ghcr.io/…:0.9); översikten visar taggen i stället.
function imageTag(image) {
  const value = (image || '').split('@')[0];
  const colon = value.lastIndexOf(':');
  return colon > value.lastIndexOf('/') ? value.slice(colon + 1) : '';
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

  // Panelen har en egen siffra i översikten: versionen och om en nyare finns.
  const panelStat = $('stat-panel');
  if (panelStat) {
    panelStat.textContent = current;
    $('stat-panel-hint').textContent = check.updateAvailable && check.latestVersion
      ? `Ny ${versionLabel(check.latestVersion)} finns`
      : (check.error ? 'Kunde inte kontrolleras' : 'Senaste versionen');
  }

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

function formatBytes(bytes, digits = 1) {
  if (!Number.isFinite(bytes) || bytes <= 0) {
    return '0 B';
  }

  const units = ['B', 'kB', 'MB', 'GB', 'TB'];
  let value = bytes;
  let index = 0;
  while (value >= 1024 && index < units.length - 1) {
    value /= 1024;
    index += 1;
  }

  return `${value.toLocaleString('sv-SE', {
    minimumFractionDigits: index === 0 ? 0 : digits,
    maximumFractionDigits: index === 0 ? 0 : digits,
  })} ${units[index]}`;
}

// Översiktens statistik kommer helt från datorn: Docker-containrarna,
// serverdata-mappen och Windows egna räknare.
function renderOverview(status) {
  const stats = status.stats ?? {};
  const host = (status.hostName || '').trim();
  const name = (status.nodeName || host || 'Den här noden').trim();

  // Nodens namn i panelen är rubriken; maskinen står kvar på raden under så
  // två agenter på olika datorer går att skilja åt.
  $('node-title').textContent = name;
  $('node-meta').textContent = host ? `Maskin ${host}` : '—';

  const total = stats.serversTotal ?? 0;
  const running = stats.serversRunning ?? 0;
  $('stat-servers').textContent = `${running}/${total}`;
  $('stat-servers-hint').textContent = total === 0
    ? 'Inga servrar ännu'
    : `${total - running} väntar på start`;

  if (stats.playersKnown) {
    $('stat-players').textContent = `${stats.playersOnline ?? 0}`;
    $('stat-players-hint').textContent = stats.playersMax
      ? `av ${stats.playersMax} platser`
      : 'online just nu';
  } else {
    $('stat-players').textContent = '—';
    $('stat-players-hint').textContent = running === 0 ? 'Inga servrar igång' : 'Servrarna svarar inte';
  }

  if (stats.memoryTotalBytes > 0) {
    $('stat-memory').textContent = `${formatBytes(stats.memoryUsedBytes)} / ${formatBytes(stats.memoryTotalBytes)}`;
    $('stat-memory-hint').textContent =
      `${Math.round((stats.memoryUsedBytes / stats.memoryTotalBytes) * 100)} % använt`;
  } else {
    $('stat-memory').textContent = '—';
    $('stat-memory-hint').textContent = 'Kunde inte läsas';
  }

  if (stats.diskTotalBytes > 0) {
    $('stat-disk').textContent = `${formatBytes(stats.diskUsedBytes)} / ${formatBytes(stats.diskTotalBytes)}`;
    $('stat-disk-hint').textContent =
      `${Math.round((stats.diskUsedBytes / stats.diskTotalBytes) * 100)} % använt`;
  } else {
    $('stat-disk').textContent = '—';
    $('stat-disk-hint').textContent = 'Kunde inte läsas';
  }

  const down = stats.networkDownBps ?? 0;
  const up = stats.networkUpBps ?? 0;
  $('stat-network').textContent = `↓ ${formatBytes(down)}/s`;
  $('stat-network-hint').textContent = `↑ ${formatBytes(up)}/s`;
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
  renderOverview(status);
  const dockerState = status.dockerState || (status.dockerOk ? 'ready' : 'error');
  state.dockerReady = dockerState === 'ready';
  state.installed = Boolean(status.installed);
  state.agentExists = Boolean(status.agentExists);

  if (state.dockerReady) {
    setCard(
      'docker-card', 'docker-dot', 'docker-badge', 'docker-value',
      'ok', 'Redo',
      status.dockerVersion ? `Server ${status.dockerVersion}` : 'Svarar'
    );
  } else if (dockerState === 'not-installed') {
    setCard(
      'docker-card', 'docker-dot', 'docker-badge', 'docker-value',
      'warn', 'Saknas',
      status.dockerDesktopInstalled ? 'Kommandot docker saknas' : 'Inte installerat'
    );
  } else if (dockerState === 'not-running') {
    setCard(
      'docker-card', 'docker-dot', 'docker-badge', 'docker-value',
      'warn', 'Stoppad',
      'Motorn kör inte'
    );
  } else {
    setCard(
      'docker-card', 'docker-dot', 'docker-badge', 'docker-value',
      'bad', 'Fel',
      shortError(status.dockerError),
      status.dockerError ?? ''
    );
  }

  setDockerNotice(status, dockerState);

  if (status.masterOk) {
    const latency = status.masterLatencyMs != null ? ` · ${status.masterLatencyMs} ms` : '';
    const version = status.masterVersion
      ? (/^v/i.test(status.masterVersion) ? status.masterVersion : `v${status.masterVersion}`)
      : 'Svarar';
    setCard(
      'master-card', 'master-dot', 'master-badge', 'master-value',
      'ok', 'Online',
      `${version}${latency}`
    );
  } else {
    setCard(
      'master-card', 'master-dot', 'master-badge', 'master-value',
      'bad', 'Fel',
      shortError(status.masterError),
      status.masterError ?? ''
    );
  }

  const tag = imageTag(status.agentImage);
  if (!state.dockerReady) {
    setCard(
      'agent-card', 'agent-dot', 'agent-badge', 'agent-value',
      'idle', 'Väntar',
      'Docker är inte igång'
    );
  } else if (status.agentRunning) {
    setCard(
      'agent-card', 'agent-dot', 'agent-badge', 'agent-value',
      'ok', 'Körs',
      tag ? versionLabel(tag) : '—',
      status.agentImage ?? ''
    );
  } else if (status.installed) {
    setCard(
      'agent-card', 'agent-dot', 'agent-badge', 'agent-value',
      'warn', 'Stoppad',
      'Containern kör inte'
    );
  } else if (status.agentExists) {
    setCard(
      'agent-card', 'agent-dot', 'agent-badge', 'agent-value',
      'warn', 'Hittad',
      'Saknar installationsfiler'
    );
  } else {
    setCard(
      'agent-card', 'agent-dot', 'agent-badge', 'agent-value',
      'idle', 'Saknas',
      'Inte installerad'
    );
  }

  if (!state.dockerReady) {
    setCard(
      'claim-card', 'claim-dot', 'claim-badge', 'claim-value',
      'idle', 'Väntar',
      'Väntar på Docker'
    );
    $('claim-code').textContent = '—';
    $('copy-claim').disabled = true;
  } else if (status.enrollmentCompleted) {
    setCard(
      'claim-card', 'claim-dot', 'claim-badge', 'claim-value',
      'ok', 'Klar',
      (status.nodeName || '').trim() || '—'
    );
    $('claim-code').textContent = 'Ansluten';
    $('copy-claim').disabled = true;
  } else if (status.claimCode) {
    setCard(
      'claim-card', 'claim-dot', 'claim-badge', 'claim-value',
      'warn', 'Väntar',
      'Väntar på koppling'
    );
    $('claim-code').textContent = status.claimCode;
    $('copy-claim').disabled = false;
  } else {
    setCard(
      'claim-card', 'claim-dot', 'claim-badge', 'claim-value',
      'idle', 'Väntar',
      'Ingen kod ännu'
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
  absent: { label: 'Inte startad', kind: 'idle' },
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
  image.textContent = server.state === 'absent'
    ? 'Väntar på första start'
    : `${server.name} · ${server.image}`;

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

  const stopped = servers.filter(
    (server) => !server.running && server.state !== 'running' && server.state !== 'absent'
  ).length;
  const prune = $('prune-servers');
  if (prune) {
    prune.hidden = stopped === 0;
    prune.textContent = stopped === 1 ? 'Rensa 1 stoppad' : `Rensa ${stopped} stoppade`;
  }
  empty.textContent = status.dockerOk === false ? 'Docker är inte igång.' : 'Inga servrar ännu.';
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
