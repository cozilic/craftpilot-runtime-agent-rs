const token = document.querySelector('meta[name="cp-token"]')?.content ?? '';
const $ = (id) => document.getElementById(id);
const i18n = window.CraftPilotI18n;
const t = (key, params) => i18n.t(key, params);
// Masterns adress: Min nod-kortet och serverraderna länkar dit.
const PANEL_URL = 'https://craftpilot.se';
const state = {
  busy: false,
  loaded: false,
  dockerReady: false,
  installed: false,
  agentExists: false,
  updating: false,
  checking: false,
  status: null,
  version: null,
  updateTimer: null,
};

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
  state.version = check;
  renderUpdateChecked(check);
  scheduleUpdateCheck(check);

  const current = versionLabel(check.currentVersion);
  const pill = $('panel-version');
  const banner = $('update-banner');
  const link = $('update-link');

  pill.textContent = current;
  pill.dataset.state = check.error ? 'error' : 'ok';
  pill.title = check.error
    ? t('version.panelTitleError', { version: current, error: check.error })
    : t('version.panelTitle', { version: current });

  // Panelen har en egen siffra i översikten: versionen och om en nyare finns.
  const panelStat = $('stat-panel');
  if (panelStat) {
    panelStat.textContent = current;
    $('stat-panel-hint').textContent = check.updateAvailable && check.latestVersion
      ? t('version.newAvailable', { version: versionLabel(check.latestVersion) })
      : (check.error
        ? t('version.checkFailed')
        : (check.checkedAt ? t('version.latest') : t('settings.neverChecked')));
  }

  // Installationsknappen i Inställningar hör till samma version som bannern.
  const installButton = $('panel-update');
  if (installButton) {
    installButton.hidden = !(check.updateAvailable && check.latestVersion);
  }
  renderUpdateResult(check);

  if (check.updateAvailable && check.latestVersion && check.releaseUrl) {
    banner.hidden = false;
    banner.dataset.state = 'available';
    $('update-dot').className = 'dot dot-warn';
    $('update-title').textContent = t('version.availableTitle', {
      version: versionLabel(check.latestVersion),
    });
    $('update-text').textContent = t('version.availableText', { current });
    link.href = check.releaseUrl;
    link.hidden = false;
    $('update-now').hidden = false;
    return;
  }

  if (check.error) {
    banner.hidden = false;
    banner.dataset.state = 'error';
    $('update-dot').className = 'dot dot-idle';
    $('update-title').textContent = t('version.errorTitle');
    $('update-text').textContent = t('version.errorText', { error: check.error });
    link.hidden = true;
    $('update-now').hidden = true;
    return;
  }

  banner.hidden = true;
  link.hidden = true;
  $('update-now').hidden = true;
}

// Raden under Kontrollera nu: vad den senaste kontrollen kom fram till.
function renderUpdateResult(check) {
  const note = $('update-result');
  if (!note) {
    return;
  }

  if (check.error) {
    note.hidden = false;
    note.dataset.state = 'error';
    note.textContent = t('version.checkFailed');
    return;
  }

  if (check.updateAvailable && check.latestVersion) {
    note.hidden = false;
    note.dataset.state = 'warn';
    note.textContent = t('version.stageFound', { version: versionLabel(check.latestVersion) });
    return;
  }

  note.hidden = !check.checkedAt;
  note.dataset.state = 'ok';
  note.textContent = check.checkedAt ? t('version.upToDate') : '';
}

async function checkVersion(force = false) {
  try {
    renderVersion(force
      ? await api('/api/version/check', { method: 'POST', body: '{}' })
      : await api('/api/version'));
  } catch (error) {
    renderVersion({ error: t('version.readFailed', { error: error.message }) });
    // Ett läst fel ska inte tystna för alltid: försök igen om tio minuter.
    scheduleUpdateCheck({ nextCheckAt: new Date(Date.now() + 10 * 60 * 1000).toISOString() });
  }
}

// När panelen själv frågar GitHub styrs av Inställningar. Servern räknar ut
// nästa tillfälle, så panelen sover tills dess i stället för att polla.
const UPDATE_INTERVALS = ['daily', 'weekly', 'manual'];

// Stegen som /api/panel-update/progress rapporterar, översatta till text i baren.
const UPDATE_STAGE_KEYS = {
  release: 'update.stageRelease',
  download: 'update.stageDownload',
  verify: 'update.stageVerify',
  install: 'update.stageInstall',
  restart: 'update.stageRestart',
};
const UPDATE_RECHECK_CAP_MS = 6 * 60 * 60 * 1000;

function renderUpdateChecked(check) {
  const label = $('update-checked');
  if (!label || !check || !('checkedAt' in check)) {
    return;
  }

  const checkedAt = check.checkedAt ? new Date(check.checkedAt) : null;
  label.textContent = checkedAt && !Number.isNaN(checkedAt.getTime())
    ? t('settings.checkedAt', {
      when: checkedAt.toLocaleString(i18n.language === 'sv' ? 'sv-SE' : 'en-GB'),
    })
    : t('settings.neverChecked');
}

function scheduleUpdateCheck(check) {
  if (state.updateTimer) {
    window.clearTimeout(state.updateTimer);
    state.updateTimer = null;
  }

  const next = check && check.nextCheckAt ? new Date(check.nextCheckAt).getTime() : NaN;
  if (Number.isNaN(next)) {
    return;
  }

  // En lång väntan kapas: servern avgör ändå om kontrollen är due, så
  // panelen behöver inte sova ett dygn i ett öppet fönster.
  const delay = Math.min(Math.max(next - Date.now(), 5000), UPDATE_RECHECK_CAP_MS);
  state.updateTimer = window.setTimeout(() => checkVersion(), delay);
}

async function loadUpdateSetting() {
  try {
    const settings = await api('/api/settings');
    const selector = $('update-interval');
    if (selector && UPDATE_INTERVALS.includes(settings.updateCheckInterval)) {
      selector.value = settings.updateCheckInterval;
    }
  } catch (error) {
    appendLog(t('log.settingsReadFailed', { error: error.message }));
  }
}

function setupUpdateSetting() {
  const selector = $('update-interval');
  if (selector) {
    selector.addEventListener('change', async () => {
      try {
        await api('/api/settings', {
          method: 'POST',
          body: JSON.stringify({ updateCheckInterval: selector.value }),
        });
        appendLog(t('log.updateIntervalSaved', {
          label: selector.options[selector.selectedIndex].textContent,
        }));
        await checkVersion();
      } catch (error) {
        appendLog(t('log.settingsSaveFailed', { error: error.message }));
      }
    });
  }

  const button = $('check-update');
  if (button) {
    // Manuell kontroll visar stegen i den gemensamma baren, som sitter ovanför
    // flikarna och därför syns även i Inställningar.
    button.addEventListener('click', async () => {
      if (state.busy || state.checking) {
        return;
      }

      state.checking = true;
      button.disabled = true;
      button.textContent = t('settings.updateChecking');
      startProgress(t('version.stageChecking'));

      try {
        renderVersion(await api('/api/version/check', { method: 'POST', body: '{}' }));
      } catch (error) {
        renderVersion({ error: t('version.readFailed', { error: error.message }) });
      } finally {
        state.checking = false;
        button.disabled = false;
        button.textContent = t('settings.updateNow');
        stopProgress();
        applyControls();
      }
    });
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

  const locale = i18n.language === 'sv' ? 'sv-SE' : 'en-GB';
  return `${value.toLocaleString(locale, {
    minimumFractionDigits: index === 0 ? 0 : digits,
    maximumFractionDigits: index === 0 ? 0 : digits,
  })} ${units[index]}`;
}

// Översiktens statistik kommer helt från datorn: Docker-containrarna,
// serverdata-mappen och Windows egna räknare.
function renderOverview(status) {
  const stats = status.stats ?? {};
  const host = (status.hostName || '').trim();
  const name = (status.nodeName || host || t('overview.thisNode')).trim();

  // Nodens namn i panelen är rubriken; maskinen står kvar på raden under så
  // två agenter på olika datorer går att skilja åt.
  $('node-title').textContent = name;
  $('node-meta').textContent = host ? t('overview.machine', { host }) : '—';

  const total = stats.serversTotal ?? 0;
  const running = stats.serversRunning ?? 0;
  $('stat-servers').textContent = `${running}/${total}`;
  $('stat-servers-hint').textContent = total === 0
    ? t('stats.noServers')
    : t('stats.waitingToStart', { count: total - running });

  if (stats.playersKnown) {
    $('stat-players').textContent = `${stats.playersOnline ?? 0}`;
    $('stat-players-hint').textContent = stats.playersMax
      ? t('stats.ofSlots', { max: stats.playersMax })
      : t('stats.onlineNow');
  } else {
    $('stat-players').textContent = '—';
    $('stat-players-hint').textContent = running === 0
      ? t('stats.noServersRunning')
      : t('stats.noResponse');
  }

  if (stats.memoryTotalBytes > 0) {
    $('stat-memory').textContent = `${formatBytes(stats.memoryUsedBytes)} / ${formatBytes(stats.memoryTotalBytes)}`;
    $('stat-memory-hint').textContent = t('stats.percentUsed', {
      percent: Math.round((stats.memoryUsedBytes / stats.memoryTotalBytes) * 100),
    });
  } else {
    $('stat-memory').textContent = '—';
    $('stat-memory-hint').textContent = t('stats.unreadable');
  }

  if (stats.diskTotalBytes > 0) {
    $('stat-disk').textContent = `${formatBytes(stats.diskUsedBytes)} / ${formatBytes(stats.diskTotalBytes)}`;
    $('stat-disk-hint').textContent = t('stats.percentUsed', {
      percent: Math.round((stats.diskUsedBytes / stats.diskTotalBytes) * 100),
    });
  } else {
    $('stat-disk').textContent = '—';
    $('stat-disk-hint').textContent = t('stats.unreadable');
  }

  const down = stats.networkDownBps ?? 0;
  const up = stats.networkUpBps ?? 0;
  $('stat-network').textContent = `↓ ${formatBytes(down)}/s`;
  $('stat-network-hint').textContent = `↑ ${formatBytes(up)}/s`;
}

function shortError(value) {
  if (!value) {
    return t('error.noDetail');
  }

  return value.split('\n').map((line) => line.trim()).find(Boolean) ?? t('error.unknown');
}

function setDockerNotice(status, dockerState) {
  const notice = $('docker-notice');
  const download = $('docker-download');
  const install = $('docker-install');
  const installStatus = $('docker-install-status');
  const cardLink = $('docker-link');

  if (dockerState === 'ready') {
    notice.hidden = true;
    download.hidden = true;
    install.hidden = true;
    installStatus.hidden = true;
    cardLink.hidden = true;
    return;
  }

  const notInstalled = dockerState === 'not-installed';
  const desktopPresent = status.dockerDesktopInstalled === true;
  notice.hidden = false;

  $('docker-notice-title').textContent = notInstalled
    ? (desktopPresent ? t('docker.missingCommandTitle') : t('docker.missingDesktopTitle'))
    : (dockerState === 'not-running' ? t('docker.notRunningTitle') : t('docker.noResponseTitle'));

  $('docker-notice-text').textContent = notInstalled
    ? (desktopPresent ? t('docker.missingCommandText') : t('docker.missingDesktopText'))
    : (dockerState === 'not-running'
      ? t('docker.notRunningText')
      : t('docker.errorText'));

  download.hidden = !notInstalled || desktopPresent;
  install.hidden = !notInstalled || desktopPresent;
  if (install.hidden) {
    installStatus.hidden = true;
  }
  cardLink.hidden = !notInstalled || desktopPresent;
}

// SFTPGo-porten agenten valde. Panelen frågar Docker i stället för att gissa
// ur .env.desktop, så en port som flyttats från 2222 syns direkt.
function renderSftp(status) {
  const sftp = status.sftp ?? {};
  const bound = sftp.boundPort;
  const preferred = sftp.preferredPort;
  const hint = $('sftp-hint');
  hint.textContent = '';

  if (sftp.enabled === false) {
    setCard(
      'sftp-card', 'sftp-dot', 'sftp-badge', 'sftp-value',
      'idle', t('sftp.disabled'), t('sftp.disabledHint')
    );
    return;
  }

  if (!state.dockerReady) {
    setCard(
      'sftp-card', 'sftp-dot', 'sftp-badge', 'sftp-value',
      'idle', t('common.waiting'), t('sftp.dockerDown')
    );
    return;
  }

  if (!sftp.running || bound == null) {
    setCard(
      'sftp-card', 'sftp-dot', 'sftp-badge', 'sftp-value',
      'idle', t('sftp.notRunning'), t('sftp.notStarted')
    );
    return;
  }

  if (preferred != null && preferred !== bound) {
    hint.textContent = t('sftp.wanted', { port: preferred });
  }

  if (sftp.responding === true) {
    setCard(
      'sftp-card', 'sftp-dot', 'sftp-badge', 'sftp-value',
      'ok', t('sftp.answering'), t('sftp.port', { port: bound })
    );
  } else {
    setCard(
      'sftp-card', 'sftp-dot', 'sftp-badge', 'sftp-value',
      'warn', t('sftp.noAnswer'), t('sftp.port', { port: bound }),
      t('sftp.probeFailed')
    );
  }
}

// Starta/Stoppa på korten. Knappen som inte kan göra något ligger disabled i
// stället för att försvinna, så korten inte hoppar mellan gångerna.
function updateCardActions() {
  const status = state.status ?? {};
  const installed = Boolean(status.installed) || Boolean(state.agentExists);
  const agentRunning = Boolean(status.agentRunning);
  const sftpEnabled = status.sftp?.enabled !== false;

  $('agent-start').disabled = state.busy || !state.dockerReady || !installed || agentRunning;
  $('agent-stop').disabled = state.busy || !state.dockerReady || !installed || !agentRunning;

  // SFTP ägs av agenten: stopp = flaggan av (containern tas bort, volymerna
  // ligger kvar), start = flaggan på igen. Agenten måste läsa om flaggan, så
  // knapparna kräver att den kör — annars skulle de starta en stoppad agent.
  $('sftp-start').disabled = state.busy || !state.dockerReady || !agentRunning || sftpEnabled;
  $('sftp-stop').disabled = state.busy || !state.dockerReady || !agentRunning || !sftpEnabled;

  // Tooltipen säger varför knapparna är låsta. Datanyckeln uppdateras också, så
  // ett språkbyte behåller rätt text.
  for (const [id, key] of [
    ['sftp-start', agentRunning ? 'card.sftpStartTitle' : 'card.sftpNeedsAgent'],
    ['sftp-stop', agentRunning ? 'card.sftpStopTitle' : 'card.sftpNeedsAgent'],
  ]) {
    $(id).dataset.i18nTitle = key;
    $(id).title = t(key);
  }
}

function renderStatus(status) {
  state.status = status;
  state.loaded = true;
  renderOverview(status);
  const dockerState = status.dockerState || (status.dockerOk ? 'ready' : 'error');
  state.dockerReady = dockerState === 'ready';
  state.installed = Boolean(status.installed);
  state.agentExists = Boolean(status.agentExists);

  if (state.dockerReady) {
    setCard(
      'docker-card', 'docker-dot', 'docker-badge', 'docker-value',
      'ok', t('common.ready'),
      status.dockerVersion ? t('docker.server', { version: status.dockerVersion }) : t('docker.responding')
    );
  } else if (dockerState === 'not-installed') {
    setCard(
      'docker-card', 'docker-dot', 'docker-badge', 'docker-value',
      'warn', t('common.missing'),
      status.dockerDesktopInstalled ? t('docker.commandMissing') : t('docker.notInstalledValue')
    );
  } else if (dockerState === 'not-running') {
    setCard(
      'docker-card', 'docker-dot', 'docker-badge', 'docker-value',
      'warn', t('common.stopped'),
      t('docker.engineDown')
    );
  } else {
    setCard(
      'docker-card', 'docker-dot', 'docker-badge', 'docker-value',
      'bad', t('common.error'),
      shortError(status.dockerError),
      status.dockerError ?? ''
    );
  }

  setDockerNotice(status, dockerState);

  if (status.masterOk) {
    const latency = status.masterLatencyMs != null ? ` · ${status.masterLatencyMs} ms` : '';
    const version = status.masterVersion
      ? (/^v/i.test(status.masterVersion) ? status.masterVersion : `v${status.masterVersion}`)
      : t('master.responding');
    setCard(
      'master-card', 'master-dot', 'master-badge', 'master-value',
      'ok', t('common.online'),
      `${version}${latency}`
    );
  } else {
    setCard(
      'master-card', 'master-dot', 'master-badge', 'master-value',
      'bad', t('common.error'),
      shortError(status.masterError),
      status.masterError ?? ''
    );
  }

  const tag = imageTag(status.agentImage);
  if (!state.dockerReady) {
    setCard(
      'agent-card', 'agent-dot', 'agent-badge', 'agent-value',
      'idle', t('common.waiting'),
      t('agent.dockerDown')
    );
  } else if (status.agentRunning) {
    setCard(
      'agent-card', 'agent-dot', 'agent-badge', 'agent-value',
      'ok', t('common.running'),
      tag ? versionLabel(tag) : '—',
      status.agentImage ?? ''
    );
  } else if (status.installed) {
    setCard(
      'agent-card', 'agent-dot', 'agent-badge', 'agent-value',
      'warn', t('common.stopped'),
      t('agent.containerDown')
    );
  } else if (status.agentExists) {
    setCard(
      'agent-card', 'agent-dot', 'agent-badge', 'agent-value',
      'warn', t('common.found'),
      t('agent.missingFiles')
    );
  } else {
    setCard(
      'agent-card', 'agent-dot', 'agent-badge', 'agent-value',
      'idle', t('common.missing'),
      t('common.notInstalled')
    );
  }

  renderSftp(status);
  updateCardActions();

  if (!state.dockerReady) {
    setCard(
      'claim-card', 'claim-dot', 'claim-badge', 'claim-value',
      'idle', t('common.waiting'),
      t('claim.waitingDocker')
    );
    $('claim-code').textContent = '—';
    $('copy-claim').disabled = true;
  } else if (status.enrollmentCompleted) {
    setCard(
      'claim-card', 'claim-dot', 'claim-badge', 'claim-value',
      'ok', t('common.done'),
      (status.nodeName || '').trim() || '—'
    );
    $('claim-code').textContent = t('common.connected');
    $('copy-claim').disabled = true;
  } else if (status.claimCode) {
    setCard(
      'claim-card', 'claim-dot', 'claim-badge', 'claim-value',
      'warn', t('common.waiting'),
      t('claim.waitingLink')
    );
    $('claim-code').textContent = status.claimCode;
    $('copy-claim').disabled = false;
  } else {
    setCard(
      'claim-card', 'claim-dot', 'claim-badge', 'claim-value',
      'idle', t('common.waiting'),
      t('claim.noCode')
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
  if (document.activeElement !== $('sftp-enabled')) {
    $('sftp-enabled').checked = status.sftp?.enabled !== false;
  }

  $('install-dir').textContent = status.installDir || '—';
  $('master-link').href = `${PANEL_URL}/node`;
  $('settings-note').textContent = status.busy ? t('common.working') : t('settings.masterLocked');

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
    appendLog(t('log.statusReadFailed', { error: error.message }));
  }
}

async function loadLogs() {
  try {
    const result = await api('/api/logs');
    $('log').textContent = result.logs || t('log.noAgentLog');
    $('log').dataset.placeholder = 'false';
    $('log').scrollTop = $('log').scrollHeight;
  } catch (error) {
    appendLog(t('log.logsReadFailed', { error: error.message }));
  }
}

function appendLog(message) {
  const log = $('log');
  const placeholder = log.dataset.placeholder !== 'false';
  const current = placeholder ? '' : log.textContent;
  log.textContent = `${current}${current ? '\n' : ''}${message}`;
  log.dataset.placeholder = 'false';
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
  $('sftp-enabled').disabled = state.busy;
  $('panel-update').disabled = state.busy || state.checking;
  updateCardActions();
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

  // Tray-menyn kan öppna en viss flik direkt (?tab=log).
  let requested = null;
  try {
    requested = new URLSearchParams(window.location.search).get('tab');
  }
  catch
  {
    // Utan URLSearchParams gäller det sparade valet.
  }

  selectTab(TAB_IDS.includes(requested) ? requested : (saved ?? 'main'));
}

// Språkvalet ligger i Inställningar. Panelen byter texter direkt och ritar om
// översikt och version, så även dynamiska rader får rätt språk.
function setupLanguage() {
  const selector = $('language');
  if (!selector) {
    return;
  }

  selector.value = i18n.language;
  selector.addEventListener('change', () => i18n.setLanguage(selector.value));
}

i18n.onChange = () => {
  i18n.applyStatic();

  const selector = $('language');
  if (selector) {
    selector.value = i18n.language;
  }

  if ($('log').dataset.placeholder !== 'false') {
    $('log').textContent = t('log.starting');
  }

  if (state.version) {
    renderVersion(state.version);
  }
  if (state.status) {
    renderStatus(state.status);
  }
  applyControls();
};

const SERVER_STATES = {
  running: { key: 'state.running', kind: 'ok' },
  restarting: { key: 'state.restarting', kind: 'warn' },
  paused: { key: 'state.paused', kind: 'warn' },
  exited: { key: 'state.exited', kind: 'idle' },
  created: { key: 'state.created', kind: 'idle' },
  dead: { key: 'state.dead', kind: 'bad' },
  absent: { key: 'state.absent', kind: 'idle' },
};

function restartServer(server) {
  const name = `Server ${server.shortId || server.serverId}`;
  if (!window.confirm(t('servers.restartConfirm', { name }))) {
    return;
  }

  // Servern hittas av panelens backend via server-id + label; inget
  // containernamn skickas från webbsidan.
  runOperation(
    `/api/servers/${encodeURIComponent(server.serverId)}/restart`,
    {},
    t('log.restartLabel'),
  );
}

function serverRow(server) {
  const info = SERVER_STATES[server.state] ?? {
    key: null,
    label: server.state ? server.state : t('common.unknown'),
    kind: 'idle',
  };

  const item = document.createElement('li');
  item.className = 'server-row';

  // Hela raden länkar till servern i panelen, där start, stopp och konsol
  // finns — listan här visar bara vad noden hostar just nu.
  const link = document.createElement('a');
  link.className = 'server-link';
  link.href = `${PANEL_URL}/servers/${encodeURIComponent(server.serverId)}`;
  link.target = '_blank';
  link.rel = 'noopener';
  link.title = t('servers.openTitle', {
    name: server.name || `Server ${server.shortId || server.serverId}`,
  });

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
    ? t('servers.waitingFirstStart')
    : `${server.name} · ${server.image}`;

  const stats = document.createElement('span');
  stats.className = 'server-stats';
  if (server.running) {
    const locale = i18n.language === 'sv' ? 'sv-SE' : 'en-GB';
    const cpu = server.cpuPercent != null
      ? `${server.cpuPercent.toLocaleString(locale, { maximumFractionDigits: 1 })} %`
      : '—';
    const memory = server.memoryUsedMb != null ? `${server.memoryUsedMb} MB` : '—';
    stats.textContent = `${cpu} · ${memory}`;
    stats.title = server.memoryLimitMb
      ? t('servers.memoryOf', { used: server.memoryUsedMb, max: server.memoryLimitMb })
      : '';
  }

  main.append(name, image, stats);

  const badge = document.createElement('span');
  badge.className = `badge badge-${info.kind}`;
  badge.textContent = info.key ? t(info.key) : info.label;

  link.append(dot, main, badge);
  item.append(link);

  // Bara en container som faktiskt körs kan startas om, och bara om raden
  // har ett server-id (kataloger utan container har inget att starta om).
  if (server.running && server.serverId) {
    const restart = document.createElement('button');
    restart.type = 'button';
    restart.className = 'button button-quiet server-restart';
    restart.textContent = t('servers.restart');
    restart.addEventListener('click', () => restartServer(server));
    item.append(restart);
  }

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
  count.title = t('servers.countTitle', { running, total: servers.length });
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
    prune.textContent = stopped === 1
      ? t('servers.pruneOne')
      : t('servers.pruneMany', { count: stopped });
  }
  empty.textContent = status.dockerOk === false ? t('servers.dockerDown') : t('servers.empty');
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
    track.setAttribute('aria-valuetext', t('progress.secondsAria', { label, seconds }));
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
  track.setAttribute('aria-valuetext', t('progress.percentAria', { label, value }));
  $('progress-elapsed').textContent = `${value} %`;
}

async function refreshDownloadProgress() {
  try {
    const progress = await api('/api/panel-update/progress');
    const stageKey = UPDATE_STAGE_KEYS[progress.stage];
    if (stageKey) {
      $('progress-label').textContent = t(stageKey);
    }

    // Bara nedladdningen vet hur många byte som kommit; övriga steg är obestämda.
    if (progress.stage === 'download' && progress.percent != null) {
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
    appendLog(t('log.dockerRequired'));
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
      $('log').dataset.placeholder = 'false';
    } else {
      appendLog(result.ok ? t('common.done') : t('log.actionFailed'));
    }

    if (!result.ok && result.error) {
      appendLog(t('log.errorPrefix', { error: result.error }));
    }

    if (result.status) {
      renderStatus(result.status);
    } else {
      await refresh(false);
    }
  } catch (error) {
    appendLog(t('log.errorPrefix', { error: error.message }));
  } finally {
    stopProgress();
    setBusy(false);
  }
}

function install() {
  if (!state.dockerReady) {
    appendLog(t('log.dockerRequiredInstall'));
    return;
  }

  const dataDir = $('data-dir').value.trim();
  runOperation('/api/install', { dataDir }, t('log.installLabel'));
}

$('install').addEventListener('click', install);
$('install-panel').addEventListener('click', install);

// Hämtar Docker Desktops installationsprogram via panelen. Filen är stor, så
// rutan följer /api/docker-install/progress och visar procent och storlek.
// Signaturkontrollen sker i panelen; misslyckas den finns webbläsarlänken kvar.
$('docker-install').addEventListener('click', async () => {
  const button = $('docker-install');
  const status = $('docker-install-status');
  button.disabled = true;
  status.hidden = false;
  status.textContent = t('docker.fetching');

  const progress = window.setInterval(async () => {
    try {
      const current = await api('/api/docker-install/progress');
      if (!current.downloading) {
        return;
      }

      const received = formatBytes(current.received);
      if (current.percent == null) {
        status.textContent = current.total > 0
          ? t('docker.fetchingBytes', { received, total: formatBytes(current.total) })
          : t('docker.fetchingReceived', { received });
        return;
      }

      status.textContent = t('docker.fetchingProgress', {
        percent: current.percent,
        received,
        total: formatBytes(current.total),
      });
    } catch {
      // Panelen svarar inte just nu; nästa intervall försöker igen.
    }
  }, 700);

  try {
    const result = await api('/api/docker-install', { method: 'POST', body: '{}' });
    status.textContent = result.message;
  } catch (error) {
    status.textContent = t('docker.fetchFailed', { error: shortError(error.message) });
  } finally {
    window.clearInterval(progress);
    button.disabled = false;
  }
});

// Hämtar panelens nästa version från GitHub, byter ut exen och startar om
// panelen. Den här sidan stängs när den nya panelen tar över.
async function runPanelUpdate() {
  if (state.busy) {
    return;
  }

  if (!window.confirm(t('update.confirm'))) {
    return;
  }

  setBusy(true);
  state.updating = true;
  startProgress(t('update.stageRelease'));
  $('update-text').textContent = t('update.downloadingText');
  $('update-now').hidden = true;
  const progressPoll = window.setInterval(refreshDownloadProgress, 400);

  try {
    const result = await api('/api/panel-update', { method: 'POST', body: '{}' });
    if (!result.ok) {
      state.updating = false;
      $('update-text').textContent = result.error ?? t('update.failed');
      $('update-now').hidden = false;
      appendLog(`\n> ${t('log.errorPrefix', {
        error: result.error ?? t('update.unknownError'),
      })}`);
      return;
    }

    $('update-dot').className = 'dot dot-ok';
    $('update-title').textContent = t('update.installedTitle', {
      version: versionLabel(result.version),
    });
    $('progress-label').textContent = t('update.stageRestart');
    $('update-text').textContent = t('update.restarting');
    appendLog(t('update.installedLog', { version: versionLabel(result.version) }));
  } catch (error) {
    state.updating = false;
    $('update-text').textContent = t('update.failedWith', { error: error.message });
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
$('panel-update').addEventListener('click', runPanelUpdate);
$('refresh').addEventListener('click', () => {
  refresh(true);
  checkVersion();
});
$('load-logs').addEventListener('click', loadLogs);
$('show-log').addEventListener('click', () => selectTab('log'));

// Städar bort servercontainrar som inte körs. Servernas filer ligger kvar på
// disken, så en start i mastern skapar en ny container med samma data.
$('prune-servers').addEventListener('click', () => {
  if (!window.confirm(t('log.pruneConfirm'))) {
    return;
  }

  runOperation('/api/servers/prune', {}, t('log.pruneLabel'));
});

$('startup').addEventListener('change', async () => {
  const enabled = $('startup').checked;
  try {
    const result = await api('/api/startup', {
      method: 'POST',
      body: JSON.stringify({ enabled }),
    });
    if (!result.ok) {
      throw new Error(result.error || t('log.startupChangeFailed'));
    }
    appendLog(enabled ? t('log.startupOn') : t('log.startupOff'));
  } catch (error) {
    $('startup').checked = !enabled;
    appendLog(t('log.startupFailedWith', { error: error.message }));
  }
});

// Slår SFTP på eller av. Avstängt läge tar bort SFTPGo-containern (volymerna
// och värdnyckeln består) och kräver att agenten startas om med den nya flaggan.
$('sftp-enabled').addEventListener('change', async () => {
  const enabled = $('sftp-enabled').checked;
  if (!enabled && !window.confirm(t('settings.sftpConfirmOff'))) {
    $('sftp-enabled').checked = true;
    return;
  }

  await runOperation(
    '/api/sftp',
    { enabled },
    enabled ? t('log.sftpOnLabel') : t('log.sftpOffLabel'),
  );
  await refresh(false);
});

$('update').addEventListener('click', () => {
  runOperation('/api/update', null, t('log.pullLabel'));
});

$('agent-start').addEventListener('click', () => {
  runOperation('/api/start', null, t('log.startLabel'));
});

$('agent-stop').addEventListener('click', () => {
  if (!window.confirm(t('log.stopConfirm'))) {
    return;
  }

  runOperation('/api/stop', null, t('log.stopLabel'));
});

$('sftp-start').addEventListener('click', () => {
  runOperation('/api/sftp', { enabled: true }, t('log.sftpOnLabel'));
});

$('sftp-stop').addEventListener('click', () => {
  if (!window.confirm(t('settings.sftpConfirmOff'))) {
    return;
  }

  runOperation('/api/sftp', { enabled: false }, t('log.sftpOffLabel'));
});

$('stop').addEventListener('click', () => {
  if (!window.confirm(t('log.stopConfirm'))) {
    return;
  }
  runOperation('/api/stop', null, t('log.stopLabel'));
});

$('cancel').addEventListener('click', async () => {
  if (!state.busy) {
    return;
  }

  try {
    const result = await api('/api/cancel', { method: 'POST', body: '{}' });
    appendLog(result.ok ? t('log.cancelling') : t('log.nothingToCancel'));
  } catch (error) {
    appendLog(t('log.cancelFailed', { error: error.message }));
  }
});

$('remove').addEventListener('click', () => {
  if (!window.confirm(t('log.removeConfirm'))) {
    return;
  }
  runOperation('/api/remove', null, t('log.removeLabel'));
});

$('open-folder').addEventListener('click', async () => {
  try {
    await api('/api/open-folder', { method: 'POST', body: '{}' });
  } catch (error) {
    appendLog(t('log.openFolderFailed', { error: error.message }));
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
    button.textContent = t('button.copied');
    window.setTimeout(() => { button.textContent = t('action.copy'); }, 1400);
  } catch {
    appendLog(t('log.copyFailed'));
  }
});

// Diagnosen samlas i panelens backend (redigerad där) och kan kopieras eller
// laddas ner som en textfil av nodägaren.
let diagnosticsText = '';

async function createDiagnostics() {
  const button = $('diagnostics');
  const original = button.textContent;
  button.disabled = true;
  button.textContent = t('diagnostics.working');

  try {
    const result = await api('/api/diagnostics');
    diagnosticsText = result.text || '';
    $('diagnostics-output').textContent = diagnosticsText;
    $('diagnostics-output').hidden = false;
    $('diagnostics-copy').hidden = false;
    $('diagnostics-download').hidden = false;
  } catch (error) {
    appendLog(t('diagnostics.failed', { error: error.message }));
  } finally {
    button.disabled = false;
    button.textContent = original;
  }
}

$('diagnostics').addEventListener('click', createDiagnostics);

$('diagnostics-copy').addEventListener('click', async () => {
  if (!diagnosticsText) {
    return;
  }

  try {
    await navigator.clipboard.writeText(diagnosticsText);
    const button = $('diagnostics-copy');
    button.textContent = t('diagnostics.copied');
    window.setTimeout(() => { button.textContent = t('diagnostics.copy'); }, 1400);
  } catch {
    appendLog(t('log.copyFailed'));
  }
});

$('diagnostics-download').addEventListener('click', () => {
  if (!diagnosticsText) {
    return;
  }

  const blob = new Blob([diagnosticsText], { type: 'text/plain;charset=utf-8' });
  const url = URL.createObjectURL(blob);
  const link = document.createElement('a');
  link.href = url;
  link.download = `craftpilot-diagnos-${new Date().toISOString().slice(0, 10)}.txt`;
  link.click();
  URL.revokeObjectURL(url);
});

$('quit').addEventListener('click', async () => {
  if (!window.confirm(t('log.quitConfirm'))) {
    return;
  }

  try {
    await api('/api/quit', { method: 'POST', body: '{}' });
  } catch {
    // The server may close before the response is read.
  }
  window.close();
});

i18n.setLanguage(i18n.detect());
setupTabs();
setupLanguage();
setupUpdateSetting();
refresh(true);
loadUpdateSetting();
checkVersion();
window.setInterval(() => {
  if (!state.busy) {
    refresh(false);
  }
}, 5000);
