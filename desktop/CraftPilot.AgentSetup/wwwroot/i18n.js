// Panelens texter på svenska och engelska. app.js hämtar strängar via
// CraftPilotI18n.t() och språkvalet i Inställningar sparas i localStorage.
// Nya nycklar ska finnas i båda språken; PanelI18nTests jämför ordlistorna.
window.CraftPilotI18n = (function () {
  const STORAGE_KEY = 'craftpilot-lang';
  const SUPPORTED = ['sv', 'en'];
  const DEFAULT_LANGUAGE = 'sv';

  const dictionary = {
    sv: {
      'localPanel': 'Lokal panel',
      'version.panelLabel': 'Panelversion',
      'version.panelTitle': 'Panelversion {version}',
      'version.panelTitleError': 'Panelversion {version}. {error}',
      'version.newAvailable': 'Ny {version} finns',
      'version.checkFailed': 'Kunde inte kontrolleras',
      'version.latest': 'Senaste versionen',
      'version.availableTitle': 'Ny panelversion {version} finns',
      'version.availableText': 'Du kör {current}. Panelen hämtar filen från GitHub, kontrollerar kontrollsumman och startar om sig själv.',
      'version.errorTitle': 'Kunde inte kontrollera panelversionen',
      'version.errorText': '{error} Klicka på Kontrollera igen för att försöka på nytt.',
      'version.readFailed': 'Kunde inte läsa versionsinformationen: {error}',

      'tabs.label': 'Panelsektioner',
      'tab.main': 'Översikt',
      'tab.settings': 'Inställningar',
      'tab.log': 'Logg',

      'common.working': 'Arbetar…',
      'common.checking': 'Kontrollerar',
      'common.checkingEllipsis': 'Kontrollerar…',
      'common.waiting': 'Väntar',
      'common.ready': 'Redo',
      'common.missing': 'Saknas',
      'common.stopped': 'Stoppad',
      'common.error': 'Fel',
      'common.online': 'Online',
      'common.running': 'Körs',
      'common.found': 'Hittad',
      'common.done': 'Klar',
      'common.connected': 'Ansluten',
      'common.notInstalled': 'Inte installerad',
      'common.unknown': 'Okänd',

      'progress.showLog': 'Visa logg',
      'progress.ariaLabel': 'Åtgärd pågår',
      'progress.ariaValue': 'Pågår',
      'progress.secondsAria': '{label}, {seconds} sekunder',
      'progress.percentAria': '{label}, {value} procent',

      'overview.thisNode': 'Den här noden',
      'overview.machine': 'Maskin {host}',
      'action.refresh': 'Kontrollera igen',
      'action.install': 'Installera / starta',
      'action.update': 'Uppdatera',
      'action.stop': 'Stoppa',
      'action.cancel': 'Avbryt',
      'action.remove': 'Ta bort',
      'action.openFolder': 'Öppna installationsmapp',
      'action.copy': 'Kopiera',
      'action.download': 'Ladda ner',
      'action.openRelease': 'Öppna releasen',
      'button.copied': 'Kopierad',

      'stats.label': 'Nodstatistik',
      'stats.servers': 'Servrar',
      'stats.players': 'Spelare',
      'stats.memory': 'Minne',
      'stats.disk': 'Disk',
      'stats.network': 'Nätverk',
      'stats.panel': 'Panel',
      'stats.noServers': 'Inga servrar ännu',
      'stats.waitingToStart': '{count} väntar på start',
      'stats.ofSlots': 'av {max} platser',
      'stats.onlineNow': 'online just nu',
      'stats.noServersRunning': 'Inga servrar igång',
      'stats.noResponse': 'Servrarna svarar inte',
      'stats.percentUsed': '{percent} % använt',
      'stats.unreadable': 'Kunde inte läsas',

      'update.title': 'Ny panelversion finns',
      'update.confirm': 'Ladda ner ny panelversion nu? Panelen startar om när filen är utbytt.',
      'update.downloadLabel': 'Laddar ner ny panelversion',
      'update.downloadingText': 'Laddar ner och kontrollerar den nya versionen…',
      'update.failed': 'Uppdateringen misslyckades.',
      'update.failedWith': 'Uppdateringen misslyckades: {error}',
      'update.unknownError': 'okänt fel',
      'update.installedTitle': 'Panel {version} är installerad',
      'update.restarting': 'Panelen startar om…',
      'update.installedLog': 'Panel {version} installerad. Panelen startar om.',

      'docker.noticeTitle': 'Docker Desktop krävs',
      'docker.download': 'Hämta Docker Desktop',
      'docker.missingCommandTitle': 'Docker-kommandot hittades inte',
      'docker.missingDesktopTitle': 'Docker Desktop saknas',
      'docker.notRunningTitle': 'Docker Desktop är inte igång',
      'docker.noResponseTitle': 'Docker svarar inte',
      'docker.missingCommandText': 'Docker Desktop finns men kommandot docker hittades inte. Starta om Docker Desktop och panelen.',
      'docker.missingDesktopText': 'Installera Docker Desktop med Linux-containrar, starta det och klicka på Kontrollera igen.',
      'docker.notRunningText': 'Starta Docker Desktop och vänta tills motorn är redo. Installationen är blockerad tills dess.',
      'docker.errorText': 'Kontrollera Docker Desktop och klicka på Kontrollera igen.',
      'docker.server': 'Server {version}',
      'docker.responding': 'Svarar',
      'docker.commandMissing': 'Kommandot docker saknas',
      'docker.notInstalledValue': 'Inte installerat',
      'docker.engineDown': 'Motorn kör inte',

      'status.label': 'Systemstatus',
      'label.docker': 'Docker',
      'label.master': 'Master',
      'label.agent': 'Agent',
      'label.claim': 'Anslutning',
      'master.responding': 'Svarar',
      'master.openNode': 'Öppna Min nod',
      'agent.dockerDown': 'Docker är inte igång',
      'agent.containerDown': 'Containern kör inte',
      'agent.missingFiles': 'Saknar installationsfiler',
      'claim.waitingDocker': 'Väntar på Docker',
      'claim.waitingLink': 'Väntar på koppling',
      'claim.noCode': 'Ingen kod ännu',

      'servers.eyebrow': 'Servrar',
      'servers.title': 'Servrar på den här noden',
      'servers.prune': 'Rensa stoppade',
      'servers.empty': 'Inga servrar ännu.',
      'servers.waitingFirstStart': 'Väntar på första start',
      'servers.countTitle': '{running} av {total} servrar körs',
      'servers.pruneOne': 'Rensa 1 stoppad',
      'servers.pruneMany': 'Rensa {count} stoppade',
      'servers.dockerDown': 'Docker är inte igång.',
      'servers.openTitle': 'Öppna {name} i panelen',
      'state.running': 'Körs',
      'state.restarting': 'Startar om',
      'state.paused': 'Pausad',
      'state.exited': 'Stoppad',
      'state.created': 'Skapad',
      'state.dead': 'Kraschad',
      'state.absent': 'Inte startad',

      'settings.title': 'Var serverna ska ligga',
      'settings.masterLocked': 'Master låst',
      'settings.dataDir': 'Serverdata på datorn',
      'settings.dataDirPlaceholder': 'C:\\Users\\du\\CraftPilot\\servers',
      'settings.hintBefore': 'Agenten ansluter alltid till ',
      'settings.hintAfter': '. Serverdata är mappen där agenten skapar och kör dina servrar. Rust-agenten kör servrarna som Docker-containrar, så Docker Desktop måste vara igång – det finns ingen Docker-fri start.',
      'settings.language': 'Språk',
      'settings.languageHint': 'Panelen byter språk direkt.',
      'settings.startupTitle': 'Starta med Windows',
      'settings.startupHint': 'Panelen startar i trayen när du loggar in.',
      'settings.installFiles': 'Installationsfiler: ',
      'settings.updateCheck': 'Sök ny panelversion',
      'settings.updateHint': 'Panelen frågar GitHub enligt valet ovan. Med Bara manuellt händer inget förrän du klickar på Kontrollera nu.',
      'settings.updateDaily': 'En gång om dagen',
      'settings.updateWeekly': 'En gång i veckan',
      'settings.updateManual': 'Bara manuellt',
      'settings.updateNow': 'Kontrollera nu',
      'settings.updateChecking': 'Kontrollerar…',
      'settings.checkedAt': 'Senast kontrollerad {when}',
      'settings.neverChecked': 'Inte kontrollerad ännu',

      'log.eyebrow': 'Senaste händelser',
      'log.title': 'Logg',
      'log.showAgentLog': 'Visa agentlogg',
      'log.starting': 'Startar panelen…',
      'log.noAgentLog': 'Ingen agentlogg ännu.',
      'log.statusReadFailed': 'Kunde inte läsa status: {error}',
      'log.logsReadFailed': 'Kunde inte läsa loggen: {error}',
      'log.dockerRequired': 'Docker Desktop måste vara igång innan den här åtgärden kan köras.',
      'log.dockerRequiredInstall': 'Docker Desktop måste vara igång innan agenten kan installeras.',
      'log.actionFailed': 'Åtgärden misslyckades.',
      'log.errorPrefix': 'Fel: {error}',
      'log.installLabel': 'Installerar och startar agenten',
      'log.pullLabel': 'Hämtar senaste agentimagen',
      'log.stopConfirm': 'Stoppa agenten? Servrarna behålls och kan startas igen senare.',
      'log.stopLabel': 'Stoppar agenten',
      'log.pruneConfirm': 'Ta bort de stoppade servercontainrarna? Servernas filer ligger kvar på disken.',
      'log.pruneLabel': 'Rensar stoppade servercontainrar',
      'log.cancelling': 'Avbryter den pågående åtgärden…',
      'log.nothingToCancel': 'Ingen pågående åtgärd att avbryta.',
      'log.cancelFailed': 'Kunde inte avbryta: {error}',
      'log.removeConfirm': 'Ta bort agenten från datorn? Containern tas bort och agenten kopplas bort från noden. Serverdata och agentens state ligger kvar.',
      'log.removeLabel': 'Tar bort agenten',
      'log.openFolderFailed': 'Kunde inte öppna mappen: {error}',
      'log.copyFailed': 'Kunde inte kopiera automatiskt. Markera koden manuellt.',
      'log.quitConfirm': 'Stäng panelen? Agenten fortsätter köra i Docker.',
      'log.startupChangeFailed': 'Kunde inte ändra automatisk start.',
      'log.startupOn': 'Panelen startar med Windows.',
      'log.startupOff': 'Automatisk start avstängd.',
      'log.startupFailedWith': 'Kunde inte ändra automatisk start: {error}',
      'log.updateIntervalSaved': 'Uppdateringskontroll: {label}.',
      'log.settingsReadFailed': 'Kunde inte läsa inställningarna: {error}',
      'log.settingsSaveFailed': 'Kunde inte spara inställningen: {error}',

      'error.noDetail': 'Ingen detalj tillgänglig.',
      'error.unknown': 'Okänt fel.',

      'footer.local': 'Panelen körs bara på den här datorn.',
      'footer.quit': 'Stäng panelen',
      'noscript': 'JavaScript måste vara aktiverat för att panelen ska fungera.',
    },
    en: {
      'localPanel': 'Local panel',
      'version.panelLabel': 'Panel version',
      'version.panelTitle': 'Panel version {version}',
      'version.panelTitleError': 'Panel version {version}. {error}',
      'version.newAvailable': '{version} is available',
      'version.checkFailed': 'Could not be checked',
      'version.latest': 'Latest version',
      'version.availableTitle': 'Panel version {version} is available',
      'version.availableText': 'You are running {current}. The panel downloads the file from GitHub, verifies the checksum and restarts itself.',
      'version.errorTitle': 'Could not check the panel version',
      'version.errorText': '{error} Click Check again to try once more.',
      'version.readFailed': 'Could not read the version information: {error}',

      'tabs.label': 'Panel sections',
      'tab.main': 'Overview',
      'tab.settings': 'Settings',
      'tab.log': 'Log',

      'common.working': 'Working…',
      'common.checking': 'Checking',
      'common.checkingEllipsis': 'Checking…',
      'common.waiting': 'Waiting',
      'common.ready': 'Ready',
      'common.missing': 'Missing',
      'common.stopped': 'Stopped',
      'common.error': 'Error',
      'common.online': 'Online',
      'common.running': 'Running',
      'common.found': 'Found',
      'common.done': 'Done',
      'common.connected': 'Connected',
      'common.notInstalled': 'Not installed',
      'common.unknown': 'Unknown',

      'progress.showLog': 'Show log',
      'progress.ariaLabel': 'Action in progress',
      'progress.ariaValue': 'In progress',
      'progress.secondsAria': '{label}, {seconds} seconds',
      'progress.percentAria': '{label}, {value} percent',

      'overview.thisNode': 'This node',
      'overview.machine': 'Machine {host}',
      'action.refresh': 'Check again',
      'action.install': 'Install / start',
      'action.update': 'Update',
      'action.stop': 'Stop',
      'action.cancel': 'Cancel',
      'action.remove': 'Remove',
      'action.openFolder': 'Open installation folder',
      'action.copy': 'Copy',
      'action.download': 'Download',
      'action.openRelease': 'Open the release',
      'button.copied': 'Copied',

      'stats.label': 'Node statistics',
      'stats.servers': 'Servers',
      'stats.players': 'Players',
      'stats.memory': 'Memory',
      'stats.disk': 'Disk',
      'stats.network': 'Network',
      'stats.panel': 'Panel',
      'stats.noServers': 'No servers yet',
      'stats.waitingToStart': '{count} waiting to start',
      'stats.ofSlots': 'of {max} slots',
      'stats.onlineNow': 'online right now',
      'stats.noServersRunning': 'No servers running',
      'stats.noResponse': 'The servers are not responding',
      'stats.percentUsed': '{percent}% used',
      'stats.unreadable': 'Could not be read',

      'update.title': 'A new panel version is available',
      'update.confirm': 'Download the new panel version now? The panel restarts once the file is replaced.',
      'update.downloadLabel': 'Downloading the new panel version',
      'update.downloadingText': 'Downloading and verifying the new version…',
      'update.failed': 'The update failed.',
      'update.failedWith': 'The update failed: {error}',
      'update.unknownError': 'unknown error',
      'update.installedTitle': 'Panel {version} is installed',
      'update.restarting': 'The panel is restarting…',
      'update.installedLog': 'Panel {version} installed. The panel is restarting.',

      'docker.noticeTitle': 'Docker Desktop is required',
      'docker.download': 'Get Docker Desktop',
      'docker.missingCommandTitle': 'The docker command was not found',
      'docker.missingDesktopTitle': 'Docker Desktop is missing',
      'docker.notRunningTitle': 'Docker Desktop is not running',
      'docker.noResponseTitle': 'Docker is not responding',
      'docker.missingCommandText': 'Docker Desktop is installed but the docker command was not found. Restart Docker Desktop and the panel.',
      'docker.missingDesktopText': 'Install Docker Desktop with Linux containers, start it and click Check again.',
      'docker.notRunningText': 'Start Docker Desktop and wait until the engine is ready. Installation is blocked until then.',
      'docker.errorText': 'Check Docker Desktop and click Check again.',
      'docker.server': 'Server {version}',
      'docker.responding': 'Responding',
      'docker.commandMissing': 'The docker command is missing',
      'docker.notInstalledValue': 'Not installed',
      'docker.engineDown': 'The engine is not running',

      'status.label': 'System status',
      'label.docker': 'Docker',
      'label.master': 'Master',
      'label.agent': 'Agent',
      'label.claim': 'Connection',
      'master.responding': 'Responding',
      'master.openNode': 'Open My node',
      'agent.dockerDown': 'Docker is not running',
      'agent.containerDown': 'The container is not running',
      'agent.missingFiles': 'Installation files are missing',
      'claim.waitingDocker': 'Waiting for Docker',
      'claim.waitingLink': 'Waiting to connect',
      'claim.noCode': 'No code yet',

      'servers.eyebrow': 'Servers',
      'servers.title': 'Servers on this node',
      'servers.prune': 'Clean up stopped',
      'servers.empty': 'No servers yet.',
      'servers.waitingFirstStart': 'Waiting for the first start',
      'servers.countTitle': '{running} of {total} servers running',
      'servers.pruneOne': 'Clean up 1 stopped',
      'servers.pruneMany': 'Clean up {count} stopped',
      'servers.dockerDown': 'Docker is not running.',
      'servers.openTitle': 'Open {name} in the panel',
      'state.running': 'Running',
      'state.restarting': 'Restarting',
      'state.paused': 'Paused',
      'state.exited': 'Stopped',
      'state.created': 'Created',
      'state.dead': 'Crashed',
      'state.absent': 'Not started',

      'settings.title': 'Where the servers live',
      'settings.masterLocked': 'Master locked',
      'settings.dataDir': 'Server data on this computer',
      'settings.dataDirPlaceholder': 'C:\\Users\\you\\CraftPilot\\servers',
      'settings.hintBefore': 'The agent always connects to ',
      'settings.hintAfter': '. Server data is the folder where the agent creates and runs your servers. The Rust agent runs the servers as Docker containers, so Docker Desktop must be running – there is no Docker-free start.',
      'settings.language': 'Language',
      'settings.languageHint': 'The panel switches language immediately.',
      'settings.startupTitle': 'Start with Windows',
      'settings.startupHint': 'The panel starts in the tray when you sign in.',
      'settings.installFiles': 'Installation files: ',
      'settings.updateCheck': 'Look for a new panel version',
      'settings.updateHint': 'The panel asks GitHub according to the choice above. With Manually only, nothing happens until you click Check now.',
      'settings.updateDaily': 'Once a day',
      'settings.updateWeekly': 'Once a week',
      'settings.updateManual': 'Manually only',
      'settings.updateNow': 'Check now',
      'settings.updateChecking': 'Checking…',
      'settings.checkedAt': 'Last checked {when}',
      'settings.neverChecked': 'Not checked yet',

      'log.eyebrow': 'Recent events',
      'log.title': 'Log',
      'log.showAgentLog': 'Show agent log',
      'log.starting': 'Starting the panel…',
      'log.noAgentLog': 'No agent log yet.',
      'log.statusReadFailed': 'Could not read the status: {error}',
      'log.logsReadFailed': 'Could not read the log: {error}',
      'log.dockerRequired': 'Docker Desktop must be running before this action can be performed.',
      'log.dockerRequiredInstall': 'Docker Desktop must be running before the agent can be installed.',
      'log.actionFailed': 'The action failed.',
      'log.errorPrefix': 'Error: {error}',
      'log.installLabel': 'Installing and starting the agent',
      'log.pullLabel': 'Pulling the latest agent image',
      'log.stopConfirm': 'Stop the agent? The servers are kept and can be started again later.',
      'log.stopLabel': 'Stopping the agent',
      'log.pruneConfirm': 'Remove the stopped server containers? The server files stay on disk.',
      'log.pruneLabel': 'Cleaning up stopped server containers',
      'log.cancelling': 'Cancelling the current action…',
      'log.nothingToCancel': 'There is no action to cancel.',
      'log.cancelFailed': 'Could not cancel: {error}',
      'log.removeConfirm': 'Remove the agent from this computer? The container is removed and the agent is disconnected from the node. Server data and the agent state remain.',
      'log.removeLabel': 'Removing the agent',
      'log.openFolderFailed': 'Could not open the folder: {error}',
      'log.copyFailed': 'Could not copy automatically. Select the code manually.',
      'log.quitConfirm': 'Close the panel? The agent keeps running in Docker.',
      'log.startupChangeFailed': 'Could not change the automatic start.',
      'log.startupOn': 'The panel starts with Windows.',
      'log.startupOff': 'Automatic start disabled.',
      'log.startupFailedWith': 'Could not change the automatic start: {error}',
      'log.updateIntervalSaved': 'Update check: {label}.',
      'log.settingsReadFailed': 'Could not read the settings: {error}',
      'log.settingsSaveFailed': 'Could not save the setting: {error}',

      'error.noDetail': 'No detail available.',
      'error.unknown': 'Unknown error.',

      'footer.local': 'The panel only runs on this computer.',
      'footer.quit': 'Close the panel',
      'noscript': 'JavaScript must be enabled for the panel to work.',
    },
  };

  function normalise(language) {
    const value = String(language || '').toLowerCase();
    return SUPPORTED.find((code) => value === code || value.startsWith(`${code}-`)) || null;
  }

  function detect() {
    let saved = null;
    try {
      saved = window.localStorage.getItem(STORAGE_KEY);
    } catch {
      // localStorage kan vara avstängt i WebView2; då gäller webbläsarens språk.
    }

    return normalise(saved) || normalise(navigator.language) || DEFAULT_LANGUAGE;
  }

  const api = {
    language: DEFAULT_LANGUAGE,
    supported: SUPPORTED,
    storageKey: STORAGE_KEY,
    onChange: null,

    t(key, params) {
      const table = dictionary[api.language] || dictionary[DEFAULT_LANGUAGE];
      let text = table[key];

      if (text === undefined) {
        text = dictionary[DEFAULT_LANGUAGE][key];
      }

      if (text === undefined) {
        return key;
      }

      if (params) {
        for (const [name, value] of Object.entries(params)) {
          text = text.split(`{${name}}`).join(String(value));
        }
      }

      return text;
    },

    // Statiska texter i index.html bär data-i18n="nyckel" (eller
    // data-i18n-title/-aria-label/-placeholder för attribut).
    applyStatic(root) {
      const scope = root || document;
      document.documentElement.lang = api.language;

      for (const node of scope.querySelectorAll('[data-i18n]')) {
        node.textContent = api.t(node.dataset.i18n);
      }
      for (const node of scope.querySelectorAll('[data-i18n-title]')) {
        node.title = api.t(node.dataset.i18nTitle);
      }
      for (const node of scope.querySelectorAll('[data-i18n-aria-label]')) {
        node.setAttribute('aria-label', api.t(node.dataset.i18nAriaLabel));
      }
      for (const node of scope.querySelectorAll('[data-i18n-aria-valuetext]')) {
        node.setAttribute('aria-valuetext', api.t(node.dataset.i18nAriaValuetext));
      }
      for (const node of scope.querySelectorAll('[data-i18n-placeholder]')) {
        node.setAttribute('placeholder', api.t(node.dataset.i18nPlaceholder));
      }
    },

    setLanguage(language) {
      api.language = normalise(language) || DEFAULT_LANGUAGE;

      try {
        window.localStorage.setItem(STORAGE_KEY, api.language);
      } catch {
        // Utan localStorage gäller valet bara så länge panelen är öppen.
      }

      api.applyStatic();

      if (typeof api.onChange === 'function') {
        api.onChange(api.language);
      }
    },

    detect,
    normalise,
  };

  return api;
})();
