const token = document.querySelector('meta[name="cp-token"]')?.content ?? '';
const $ = (id) => document.getElementById(id);
const state = { busy: false, loaded: false };

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

function shortError(value) {
  if (!value) {
    return 'Ingen detalj tillgänglig.';
  }

  return value.split('\n').map((line) => line.trim()).find(Boolean) ?? 'Okänt fel.';
}

function renderStatus(status) {
  state.loaded = true;

  if (status.dockerOk) {
    setCard(
      'docker-card', 'docker-dot', 'docker-badge', 'docker-title', 'docker-detail',
      'ok', 'Redo',
      'Docker är redo',
      `Server ${status.dockerVersion || 'tillgänglig'}`
    );
  } else {
    setCard(
      'docker-card', 'docker-dot', 'docker-badge', 'docker-title', 'docker-detail',
      'bad', 'Fel',
      'Docker svarar inte',
      shortError(status.dockerError) + ' Starta Docker Desktop och försök igen.'
    );
  }

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

  if (status.agentRunning) {
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
      'Ange master och klicka på Installera / starta.'
    );
  }

  if (status.enrollmentCompleted) {
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

  if (document.activeElement !== $('master-url')) {
    $('master-url').value = status.masterUrl || '';
  }
  if (document.activeElement !== $('data-dir')) {
    $('data-dir').value = status.dataDir || '';
  }

  $('install-dir').textContent = status.installDir || '—';
  $('master-link').href = `${(status.masterUrl || 'https://craftpilot.se').replace(/\/$/, '')}/node`;
  $('settings-note').textContent = status.busy ? 'Arbetar…' : 'Sparas lokalt';

  if (status.busy !== state.busy) {
    setBusy(status.busy);
  }
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

function setBusy(busy) {
  state.busy = busy;
  document.body.classList.toggle('is-busy', busy);
  for (const id of ['install', 'install-panel', 'update', 'stop', 'refresh', 'open-folder', 'load-logs']) {
    const button = $(id);
    if (button) {
      button.disabled = busy;
    }
  }
}

async function runOperation(path, body, label) {
  if (state.busy) {
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
  const masterUrl = $('master-url').value.trim();
  const dataDir = $('data-dir').value.trim();
  runOperation('/api/install', { masterUrl, dataDir }, 'Installerar och startar agenten');
}

$('install').addEventListener('click', install);
$('install-panel').addEventListener('click', install);
$('refresh').addEventListener('click', () => refresh(true));
$('load-logs').addEventListener('click', loadLogs);

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
window.setInterval(() => {
  if (!state.busy) {
    refresh(false);
  }
}, 5000);
