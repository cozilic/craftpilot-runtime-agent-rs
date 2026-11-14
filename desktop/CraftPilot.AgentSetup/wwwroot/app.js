const token = document.querySelector('meta[name="cp-token"]')?.content ?? '';
const $ = (id) => document.getElementById(id);
const state = { busy: false, loaded: false, dockerReady: false, installed: false };

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
    $('update-text').textContent = `Du kör ${current}. Ladda ner den nya versionen från GitHub Releases.`;
    link.href = check.releaseUrl;
    link.hidden = false;
    return;
  }

  if (check.error) {
    banner.hidden = false;
    banner.dataset.state = 'error';
    $('update-dot').className = 'dot dot-idle';
    $('update-title').textContent = 'Kunde inte kontrollera panelversionen';
    $('update-text').textContent = `${check.error} Klicka på Kontrollera igen för att försöka på nytt.`;
    link.hidden = true;
    return;
  }

  banner.hidden = true;
  link.hidden = true;
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
  for (const id of ['update', 'stop']) {
    $(id).disabled = !canManage;
  }
  $('refresh').disabled = state.busy;
  $('open-folder').disabled = state.busy;
  $('startup').disabled = state.busy;
  $('load-logs').disabled = state.busy || !state.installed;
}

function setBusy(busy) {
  state.busy = busy;
  document.body.classList.toggle('is-busy', busy);
  applyControls();
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
$('refresh').addEventListener('click', () => {
  refresh(true);
  checkVersion();
});
$('load-logs').addEventListener('click', loadLogs);

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

refresh(true);
checkVersion();
window.setInterval(() => {
  if (!state.busy) {
    refresh(false);
  }
}, 5000);
